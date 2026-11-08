//! Process-independent COM activation and call queues. Guest addresses never
//! cross this boundary; objects are exported under server-owned reference IDs.
use std::collections::{BTreeMap, VecDeque};
#[cfg(test)]
use super::enc::Enc;
#[derive(Clone, Debug)]
pub struct Class { pub pid: u32, pub tid: u32, pub cookie: u32, pub flags: u32, pub suspended: bool }
#[derive(Clone, Debug, Default)]
pub struct Call {
    pub id: u32, pub from: u32, pub to: u32, pub tid: u32,
    /// 0 activation, 1 method, 2 QueryInterface, 3 AddRef, 4 Release
    pub op: u32, pub object: u32, pub iid: Vec<u8>, pub class: String,
    pub method: u32, pub data: Vec<u8>,
}
#[derive(Clone, Debug)]
pub struct Reply { pub hr: u32, pub data: Vec<u8> }
#[derive(Default)]
pub struct Broker {
    pub classes: BTreeMap<String, Class>,
    pub calls: VecDeque<Call>,
    pub replies: BTreeMap<u32, Reply>,
    next: u32,
}
impl Broker {
    pub fn submit(&mut self, mut call: Call) -> u32 {
        self.next += 1; call.id = self.next; self.calls.push_back(call); self.next
    }
    pub fn take(&mut self, pid: u32, tid: u32) -> Option<Call> {
        let i = self.calls.iter().position(|c| c.to == pid && c.tid == tid)?;
        self.calls.remove(i)
    }
    pub fn exited(&mut self, pid: u32) {
        self.classes.retain(|_, c| c.pid != pid);
        self.calls.retain(|c| {
            if c.from == pid { return false; }
            if c.to == pid { self.replies.insert(c.id, Reply { hr: 0x8001_0108, data: vec![] }); return false; }
            true
        });
    }
}
crate::enc_struct!(Class { pid, tid, cookie, flags, suspended });
crate::enc_struct!(Call { id, from, to, tid, op, object, iid, class, method, data });
crate::enc_struct!(Reply { hr, data });
crate::enc_struct!(Broker { classes, calls, replies, next });

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calls_are_isolated_and_responses_can_arrive_out_of_order() {
        let mut b = Broker::default();
        let first = b.submit(Call { from: 1, to: 2, tid: 20, ..Default::default() });
        let second = b.submit(Call { from: 3, to: 2, tid: 20, ..Default::default() });
        assert!(b.take(2, 21).is_none());
        assert_eq!(b.take(2, 20).unwrap().id, first);
        assert_eq!(b.take(2, 20).unwrap().id, second);
        b.replies.insert(second, Reply { hr: 2, data: vec![2] });
        b.replies.insert(first, Reply { hr: 1, data: vec![1] });
        assert_eq!(b.replies.remove(&first).unwrap().data, [1]);
        assert_eq!(b.replies.remove(&second).unwrap().data, [2]);
    }
    #[test]
    fn death_unregisters_classes_and_fails_pending_calls() {
        let mut b = Broker::default();
        b.classes.insert("test".into(), Class { pid: 2, tid: 20, cookie: 1, flags: 1, suspended: false });
        let id = b.submit(Call { from: 1, to: 2, tid: 20, ..Default::default() });
        let dead_client = b.submit(Call { from: 2, to: 1, tid: 10, ..Default::default() });
        b.exited(2);
        assert!(b.classes.is_empty()); assert!(b.calls.is_empty());
        assert_eq!(b.replies[&id].hr, 0x8001_0108);
        assert!(!b.replies.contains_key(&dead_client));
        let mut bytes = vec![]; b.put(&mut bytes);
        let b = Broker::get(&mut bytes.as_slice()).unwrap();
        assert_eq!(b.replies[&id].hr, 0x8001_0108);
    }
}
