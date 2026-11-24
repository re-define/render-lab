//! Export aligned pass scopes and the timeline points actually submitted to Vulkan.
use super::PassReport;
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Follow the latest-finishing observed predecessor through graph, queue and submit edges.
fn completion_chain(passes: &[PassReport]) -> BTreeSet<usize> {
    let mut last_queue = BTreeMap::new();
    let mut last_batch = BTreeMap::new();
    let mut parents = Vec::with_capacity(passes.len());
    let end = |i: usize| passes[i].interval.unwrap()[1];
    for (i, p) in passes.iter().enumerate() {
        let b = &p.batch;
        let pred = p
            .parents
            .iter()
            .copied()
            .chain(last_queue.get(&b.queue).copied())
            .chain(
                b.waits
                    .iter()
                    .filter_map(|wait| last_batch.get(wait).copied()),
            )
            .max_by(|&l, &r| end(l).total_cmp(&end(r)));
        parents.push(pred);
        last_queue.insert(b.queue, i);
        last_batch.insert((b.queue, b.signal), i);
    }
    let mut tip = (0..passes.len()).max_by(|&l, &r| end(l).total_cmp(&end(r)));
    let mut chain = BTreeSet::new();
    while let Some(i) = tip {
        chain.insert(i);
        tip = parents[i];
    }
    chain
}
pub fn export(passes: &[PassReport], path: &Path) -> Result<()> {
    ensure!(
        !passes.is_empty(),
        "wait for a completed frame before exporting"
    );
    ensure!(
        passes.iter().all(|p| p.interval.is_some()),
        "device does not expose a common GPU clock across active queues"
    );
    let mut events = Vec::new();
    let mut bounds = BTreeMap::<(usize, u64), [f64; 2]>::new();
    let chain = completion_chain(passes);
    for (i, p) in passes.iter().enumerate() {
        let [start, end] = p.interval.unwrap();
        let b = &p.batch;
        let range = bounds.entry((b.queue, b.signal)).or_insert([start, end]);
        range[0] = range[0].min(start);
        range[1] = range[1].max(end);
        events.push(json!({"ph":"X","pid":1,"tid":b.queue,"ts":start*1000.,"dur":(end-start)*1000.,
          "name":p.name,"cat":p.phase.key(),"args":{"pass_index":i,"completion_chain":chain.contains(&i),"signal":b.signal,"waits":b.waits,"parents":p.parents,"barriers":p.barriers}}));
    }
    for q in bounds.keys().map(|k| k.0).collect::<BTreeSet<_>>() {
        events.push(json!({"ph":"M","name":"thread_name","pid":1,"tid":q,"args":{"name":format!("GPU queue {q}")}}));
    }
    let mut id = 0;
    for (i, p) in passes.iter().enumerate() {
        let b = &p.batch;
        if i > 0 && std::sync::Arc::ptr_eq(&passes[i - 1].batch, b) {
            continue;
        }
        let destination = bounds[&(b.queue, b.signal)][0] * 1000.;
        for &(q, value) in &b.waits {
            let name = format!("Q{q} signal {value} → Q{}", b.queue);
            if let Some(source) = bounds.get(&(q, value))
                && source[1] * 1000. <= destination
            {
                // Draw a flow only when both timestamp endpoints are ordered. A staged
                // wait inside overlapping scopes is shown as a marker on its consumer.
                id += 1;
                events.push(json!({"ph":"s","pid":1,"tid":q,"id":id,"ts":source[1]*1000.,"name":name,"cat":"wait"}));
                events.push(json!({"ph":"f","pid":1,"tid":b.queue,"id":id,"ts":destination,"name":name,"cat":"wait","bp":"e"}));
            } else {
                events.push(json!({"ph":"i","s":"t","pid":1,"tid":b.queue,"ts":destination,"name":name,"cat":"wait","args":{"prior_frame":!bounds.contains_key(&(q,value))}}));
            }
        }
    }
    use std::io::Write;
    let mut output = std::io::BufWriter::new(std::fs::File::create(path)?);
    serde_json::to_writer(
        &mut output,
        &json!({"traceEvents":events,"displayTimeUnit":"ms","completionChain":chain}),
    )?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdg::{PassPhase, Queue};
    use crate::rhi::frame::QueueBatch;

    #[test]
    fn distinguishes_ordered_waits_from_overlapping_and_prior_frame_waits() -> Result<()> {
        let pass = |queue, signal, waits, start, end| PassReport {
            batch: std::sync::Arc::new(QueueBatch {
                queue,
                signal,
                waits,
            }),
            interval: Some([start, end]),
            phase: PassPhase::Compute,
            queue: Queue::Compute,
            name: format!("Q{queue}"),
            parents: Vec::new(),
            barriers: Vec::new(),
            ms: end - start,
            raster: None,
        };
        let mut passes = [
            pass(0, 24, vec![], 0., 2.),
            pass(1, 15, vec![(0, 24), (2, 6)], 3., 6.),
            pass(2, 7, vec![(1, 15)], 5., 8.),
        ];
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("timeline.json");
        export(&passes, &path)?;
        let trace: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let events = trace["traceEvents"].as_array().unwrap();
        let waits: Vec<_> = events.iter().filter(|e| e["cat"] == "wait").collect();
        assert_eq!(waits.len(), 4);
        assert_eq!(waits[0]["ph"], "s");
        assert_eq!(waits[0]["ts"], 2000.);
        assert_eq!(waits[1]["ph"], "f");
        assert_eq!(waits[1]["ts"], 3000.);
        assert_eq!(waits[0]["id"], waits[1]["id"]);
        assert_eq!(waits[2]["args"]["prior_frame"], true);
        assert_eq!(waits[3]["args"]["prior_frame"], false);
        assert_eq!(trace["completionChain"], json!([0, 1, 2]));
        passes[2].interval = None;
        assert!(export(&passes, &path).is_err());
        Ok(())
    }
}
