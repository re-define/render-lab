use render_lab::rhi::Frame;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

const WARMUP: usize = 60;
pub(crate) struct Benchmark {
    target: usize,
    seen: usize,
    previous: Option<(Instant, u64)>,
    first: Option<(Instant, u64)>,
    series: BTreeMap<String, Vec<f64>>,
    pass_ms: BTreeMap<String, Vec<f64>>,
    frame_times: Vec<Value>,
}
fn summarize(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let q = |x: f64| samples[((samples.len() - 1) as f64 * x).round() as usize];
    json!({"min":q(0.),"median":q(0.5),"p95":q(0.95),"max":q(1.),"mean":samples.iter().sum::<f64>()/samples.len() as f64})
}
impl Benchmark {
    pub fn new(target: usize) -> Self {
        Self {
            target,
            seen: 0,
            first: None,
            previous: None,
            series: Default::default(),
            pass_ms: Default::default(),
            frame_times: Vec::new(),
        }
    }
    pub fn observe(&mut self, f: &Frame) -> bool {
        if f.profile.wsi_binary_pairs == 0 {
            return false;
        }
        self.seen += 1;
        let prev = self.previous.replace((f.completed, f.sequence));
        if self.seen <= WARMUP {
            if self.seen == WARMUP {
                self.first = self.previous;
            }
            return false;
        }
        let (previous, sequence) = prev.unwrap();
        self.frame_times.push(json!({"tick":f.profile.tick_id,"snapshot":f.profile.snapshot_id,
            "submission":f.profile.submission_id,"completion":f.sequence,"skipped_ticks_total":f.profile.skipped_render_ticks,
            "cpu_us":f.profile.cpu_times_us,"gpu_span_ms":f.profile.gpu_span_ms}));
        if sequence + 1 == f.sequence {
            self.series
                .entry("frame_interval_ms".into())
                .or_default()
                .push((f.completed - previous).as_secs_f64() * 1000.);
        }
        for (key, value) in serde_json::to_value(&f.profile)
            .unwrap()
            .as_object()
            .unwrap()
        {
            if (key.ends_with("_ms")
                || matches!(key.as_str(), "overlapping_recordings" | "recording_workers"))
                && let Some(n) = value.as_f64()
            {
                self.series.entry(key.clone()).or_default().push(n);
            }
        }
        let mut phases = BTreeMap::new();
        for p in &f.passes {
            self.pass_ms.entry(p.name.clone()).or_default().push(p.ms);
            *phases.entry(p.phase).or_insert(0.) += p.ms;
        }
        for (phase, ms) in phases {
            let phase = phase.key();
            self.series
                .entry(format!("gpu_{phase}_sum_ms"))
                .or_default()
                .push(ms);
        }
        if self.seen < WARMUP + self.target {
            return false;
        }
        let (start, seq) = self.first.unwrap();
        let dic = |table: BTreeMap<String, Vec<f64>>| {
            table
                .into_iter()
                .map(|(k, v)| (k, summarize(v)))
                .collect::<BTreeMap<_, _>>()
        };
        let rows=f.passes.iter().map(|p|json!({"name":p.name,"phase":p.phase.key(),"queue":format!("{:?}",p.queue),"physical_queue":p.batch.queue,"signal":p.batch.signal,"waits":p.batch.waits,"interval_ms":p.interval,"parents":p.parents,"resource_barriers":p.barriers.len()})).collect::<Vec<_>>();
        println!(
            "BENCHMARK {}",
            json!({"gpu":f.gpu,"extent":f.extent,"mode":f.presentation,"profile":if cfg!(debug_assertions){"dev"}else{"release"},"warmup":WARMUP,"samples":self.target,
            "presented":f.sequence-seq,"elapsed_s":(f.completed-start).as_secs_f64(),"frame_times":self.frame_times,"present_requests_per_s":(f.sequence-seq) as f64/(f.completed-start).as_secs_f64(),
            "first_frame":&*f.first_profile,"last":f.profile,"memory":f.memory,"submissions":f.submissions,"counters":f.counters,"stats":dic(std::mem::take(&mut self.series)),"gpu_pass_ms":dic(std::mem::take(&mut self.pass_ms)),"passes":rows})
        );
        true
    }
}
