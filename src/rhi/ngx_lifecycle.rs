//! Serialize NGX entry points without holding an engine thread in a driver shutdown.
use anyhow::{Result, ensure};
use std::{
    sync::{
        Condvar, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

struct Calls {
    busy: bool,
    closing: usize,
    stalled: bool,
}
static CALLS: Mutex<Calls> = Mutex::new(Calls {
    busy: false,
    closing: 0,
    stalled: false,
});
static CHANGED: Condvar = Condvar::new();

pub(super) struct ApiCall {
    closing: bool,
}
impl ApiCall {
    pub fn enter() -> Result<Self> {
        let calls = CALLS.lock().unwrap();
        let mut calls = CHANGED
            .wait_while(calls, |s| !s.stalled && (s.busy || s.closing != 0))
            .unwrap();
        ensure!(!calls.stalled, "previous NGX device is still shutting down");
        calls.busy = true;
        Ok(Self { closing: false })
    }
    fn for_retirement() -> Self {
        let calls = CALLS.lock().unwrap();
        let mut calls = CHANGED.wait_while(calls, |s| s.busy).unwrap();
        calls.busy = true;
        Self { closing: true }
    }
}
impl Drop for ApiCall {
    fn drop(&mut self) {
        let mut calls = CALLS.lock().unwrap();
        calls.busy = false;
        if self.closing {
            calls.closing -= 1;
            if calls.closing == 0 {
                calls.stalled = false;
            }
        }
        CHANGED.notify_all();
    }
}

pub(super) trait Close: Send + 'static {
    fn close(self);
}
pub(super) struct Retirement<T> {
    send: Sender<T>,
    finished: Mutex<Receiver<()>>,
}
impl<T: Close> Retirement<T> {
    pub fn new() -> Result<Self> {
        let (send, jobs) = mpsc::channel::<T>();
        let (done, finished) = mpsc::channel();
        // Provision before initializing the SDK so a thread creation failure needs no vendor rollback.
        thread::Builder::new()
            .name("lab-ngx-shutdown".into())
            .spawn(move || {
                if let Ok(session) = jobs.recv() {
                    {
                        let _call = ApiCall::for_retirement();
                        session.close();
                    }
                    let _ = done.send(());
                }
            })?;
        Ok(Self {
            send,
            finished: Mutex::new(finished),
        })
    }

    pub fn close(&mut self, session: T) {
        self.close_with_timeout(session, Duration::from_secs(2));
    }

    fn close_with_timeout(&mut self, session: T, timeout: Duration) {
        CALLS.lock().unwrap().closing += 1;
        CHANGED.notify_all();
        self.send
            .send(session)
            .expect("NGX retirement worker exited before receiving its session");
        if let Err(mpsc::RecvTimeoutError::Timeout) =
            self.finished.get_mut().unwrap().recv_timeout(timeout)
        {
            let mut calls = CALLS.lock().unwrap();
            if calls.closing == 0 {
                return;
            }
            calls.stalled = true;
            CHANGED.notify_all();
            drop(calls);
            eprintln!(
                "NVIDIA NGX shutdown is still pending in the driver; its Vulkan device is retained until shutdown returns."
            );
        }
    }
}

#[cfg(all(test, not(render_lab_ngx)))]
mod tests {
    use super::*;

    struct Driver(mpsc::Receiver<()>, mpsc::Sender<()>);
    impl Close for Driver {
        fn close(self) {
            self.0.recv().unwrap();
            self.1.send(()).unwrap();
        }
    }

    #[test]
    fn stalled_driver_keeps_its_owner_and_reopens_after_retirement() -> Result<()> {
        let (release, blocker) = mpsc::channel();
        let (retired, done) = mpsc::channel();
        let mut worker = Retirement::new()?;
        worker.close_with_timeout(Driver(blocker, retired), Duration::ZERO);
        assert!(ApiCall::enter().is_err());
        assert!(done.try_recv().is_err());
        release.send(())?;
        done.recv_timeout(Duration::from_secs(2))?;
        worker
            .finished
            .get_mut()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))?;
        drop(ApiCall::enter()?);
        let (ready, released) = mpsc::channel();
        let (closed, confirmation) = mpsc::channel();
        ready.send(())?;
        Retirement::new()?.close(Driver(released, closed));
        confirmation.try_recv()?;
        Ok(())
    }
}
