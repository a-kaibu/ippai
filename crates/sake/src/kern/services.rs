//! Service records shared by every process in a prefix's running kernel.
use std::collections::{BTreeMap, VecDeque};
#[cfg(test)]
use super::enc::Enc;

#[derive(Clone, Debug)]
pub struct Service {
    pub name: String,
    pub display: String,
    pub ty: u32,
    pub start: u32,
    pub path: String,
    pub dependencies: Vec<String>,
    /// SERVICE_STATUS's seven DWORDs; initialized STOPPED, never inferred RUNNING.
    pub status: Vec<u32>,
    pub pid: u32,
    pub args: Vec<String>,
    pub connected: bool,
    pub deleted: bool,
    pub refs: BTreeMap<u32, u32>,
    pub controls: VecDeque<(u32, u32)>,
    pub replies: BTreeMap<u32, u32>,
    pub next_control: u32,
}
impl Service {
    pub fn new(name: String, display: String, ty: u32, start: u32, path: String) -> Self {
        Self { name, display, ty, start, path, dependencies: vec![], status: vec![ty, 1, 0, 0, 0, 0, 0], pid: 0,
            args: Vec::new(), connected: false, deleted: false, refs: BTreeMap::new(),
            controls: VecDeque::new(), replies: BTreeMap::new(), next_control: 0 }
    }
    pub fn begin_start(&mut self, args: Vec<String>) -> Result<(), u32> {
        if self.deleted { return Err(1072); }
        if self.ty & 3 != 0 { return Err(1275); }
        if self.start == 4 { return Err(1058); }
        if self.status[1] != 1 { return Err(1056); }
        self.status = vec![self.ty, 2, 0, 0, 0, 0, 30000];
        self.args = args;
        self.connected = false;
        self.pid = 0;
        self.controls.clear();
        self.replies.clear();
        Ok(())
    }
    pub fn report(&mut self, pid: u32, status: Vec<u32>) -> Result<(), u32> {
        if pid == 0 || self.pid != pid { return Err(6); }
        if status.len() != 7 || !(1..=7).contains(&status[1]) || status[0] & !0x100 != self.ty & !0x100 { return Err(13); }
        self.status = status;
        if self.status[1] == 1 { self.pid = 0; self.connected = false; }
        Ok(())
    }
    pub fn exited(&mut self, pid: u32) {
        self.refs.remove(&pid);
        if self.pid == pid {
            self.status = vec![self.ty, 1, 0, 1067, 0, 0, 0];
            self.pid = 0;
            self.connected = false;
            self.controls.clear();
        }
    }
    /// The next control for the dispatcher in process `pid` (None: none yet), or Err once it has
    /// no more to serve: the service stopped, or runs in another process (started again before
    /// this dispatcher saw its STOPPED: the controls are the new process's)
    pub fn control_for(&mut self, pid: u32) -> Result<Option<(u32, u32)>, ()> {
        if self.pid != pid || self.status[1] == 1 && self.controls.is_empty() { return Err(()); }
        Ok(self.controls.pop_front())
    }
    pub fn removable(&self) -> bool { self.deleted && self.status[1] == 1 && self.refs.is_empty() }
}
crate::enc_struct!(Service { name, display, ty, start, path, dependencies, status, pid, args, connected,
    deleted, refs, controls, replies, next_control });

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Service { Service::new("test".into(), "Test".into(), 0x10, 3, "test.exe".into()) }
    #[test]
    fn start_requires_report_and_cannot_race() {
        let mut s = service();
        assert_eq!(s.begin_start(vec!["argument".into()]), Ok(()));
        assert_eq!(s.status[1], 2);
        assert_eq!(s.begin_start(vec![]), Err(1056));
        s.pid = 7;
        assert_eq!(s.report(8, vec![0x10, 4, 1, 0, 0, 0, 0]), Err(6));
        assert_eq!(s.status[1], 2);
        assert_eq!(s.report(7, vec![0x10, 4, 1, 0, 0, 0, 0]), Ok(()));
        assert_eq!(s.status[1], 4);
    }
    #[test]
    fn stop_restart_and_old_process_exit() {
        let mut s = service(); s.begin_start(vec![]).unwrap(); s.pid = 7;
        s.report(7, vec![0x10, 1, 0, 0, 0, 0, 0]).unwrap();
        s.begin_start(vec![]).unwrap(); s.pid = 9;
        s.exited(7);
        assert_eq!(s.pid, 9);
        assert_eq!(s.status[1], 2);
        s.exited(9);
        assert_eq!(s.status[1], 1);
        assert_eq!(s.status[3], 1067);
    }
    #[test]
    fn a_restart_takes_the_controls_from_the_old_dispatcher() {
        let mut s = service(); s.begin_start(vec![]).unwrap(); s.pid = 7;
        s.controls.push_back((1, 128));
        assert_eq!(s.control_for(7), Ok(Some((1, 128))));
        assert_eq!(s.control_for(7), Ok(None));
        // (stopped, then started again before process 7's dispatcher looked)
        s.report(7, vec![0x10, 1, 0, 0, 0, 0, 0]).unwrap();
        s.begin_start(vec![]).unwrap(); s.pid = 9;
        s.report(9, vec![0x10, 4, 1, 0, 0, 0, 0]).unwrap();
        s.controls.push_back((2, 1));
        assert_eq!(s.control_for(7), Err(()));
        assert_eq!(s.controls.len(), 1);
        assert_eq!(s.control_for(9), Ok(Some((2, 1))));
        s.report(9, vec![0x10, 1, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(s.control_for(9), Err(()));
    }
    #[test]
    fn deletion_waits_for_handles_and_process() {
        let mut s = service(); s.begin_start(vec![]).unwrap(); s.pid = 7;
        s.refs.insert(1, 1); s.deleted = true;
        assert_eq!(s.begin_start(vec![]), Err(1072));
        assert!(!s.removable());
        s.exited(7); assert!(!s.removable());
        s.exited(1); assert!(s.removable());
    }
    #[test]
    fn shared_state_roundtrip_and_crash() {
        let mut s = service(); s.begin_start(vec!["hello".into()]).unwrap(); s.pid = 7;
        s.controls.push_back((1, 128));
        let mut b = Vec::new(); s.put(&mut b);
        let mut s = Service::get(&mut b.as_slice()).unwrap();
        assert_eq!(s.controls.pop_front(), Some((1, 128)));
        assert_eq!(s.args, ["hello"]);
        s.exited(7); assert_eq!(s.status[3], 1067);
    }
}
