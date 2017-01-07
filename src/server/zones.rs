//! Access to zone data.

use std::io;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use futures::{Async, Done, done};
use ::bits::{ComposeMode, MessageBuf, MessageBuilder, Question, Record,
             RecordData};
use ::bits::name::{DName, DNameBuf, Labelette};
use ::iana::{Class, Rcode, Rtype};
use ::master::FileReaderIter;
use ::rdata::MasterRecordData;
use ::rdata::owned::Ns;
use super::service::NameService;


//------------ AuthoritativeZones --------------------------------------------

#[derive(Clone, Debug)]
pub struct AuthoritativeZones {
    /// The root node for the IN class.
    in_root: Node<Option<Zone>>,

    /// The root nodes for all the other classes.
    roots: HashMap<Class, Node<Option<Zone>>>,
}

impl AuthoritativeZones {
    pub fn new() -> Self {
        AuthoritativeZones {
            in_root: Node::new(None),
            roots: HashMap::new()
        }
    }

    pub fn add_zone<N: DName>(&mut self, name: &N, class: Class, zone: Zone)
                    -> Result<(), ()> {
        let node = {
            let mut iter = name.labelettes().rev();
            assert!(iter.next().unwrap().is_root());
            let node = try!(self.root_mut(class)
                                .build_node(iter, |_| Ok(None)));
            if node.value().is_some() {
                return Err(())
            }
            node
        };
        *node.value_mut() = Some(zone);
        Ok(())
    }

    pub fn load_zone<N: DName>(&mut self, name: &N, class: Class,
                               records: FileReaderIter) -> Result<(), ()> {
        let mut zone = Zone::new();
        let mut errs = Vec::new();
        for record in records {
            match record {
                Ok(record) => {
                    let name = match record.owner.strip_suffix(name) {
                        Ok(name) => name,
                        Err(_) => {
                            // XXX push error
                            continue
                        }
                    };
                    match zone.add_record(&name, record.ttl,
                                          record.rdata) {
                        Ok(_) => { }
                        Err(_) => {
                            // XXX push error
                            continue
                        }
                    }
                }
                Err(err) => errs.push(err),
            }
        }
        if errs.is_empty() {
            self.add_zone(name, class, zone)
        }
        else {
            // XXX ...
            Err(())
        }
    }
}

impl AuthoritativeZones {
    pub fn query<N: DName>(&self, question: &Question<N>)
                           -> Option<Entry<Option<&RRset<MasterRecordData>>,
                                           &Cut>> {
        let (zone, iter) = match self.find(question.qclass(),
                                           question.qname().labelettes()
                                                           .rev()) {
            Some(x) => x,
            None => return None
        };
        zone.query(iter, question.qtype())
    }

    pub fn find<'a, I>(&self, class: Class, mut iter: I)
                       -> Option<(&Zone, I)>
                where I: Iterator<Item=Labelette<'a>> + Clone {
        assert!(iter.next().unwrap().is_root());
        let mut node = match self.root(class) {
            Some(node) => node,
            None => return None
        };
        let mut apex = None;
        loop {
            if let Some(ref zone) = *node.value() {
                apex = Some((zone, iter.clone()))
            }
            let ltte = match iter.next() {
                Some(ltte) => ltte,
                None => return apex
            };
            let child = match node.get_child(ltte) {
                Some(child) => child,
                None => break
            };
            node = child;
        }
        apex
    }
}

impl AuthoritativeZones {
    fn root(&self, class: Class) -> Option<&Node<Option<Zone>>> {
        if class == Class::In { Some(&self.in_root) }
        else { self.roots.get(&class) }
    }

    fn root_mut(&mut self, class: Class) -> &mut Node<Option<Zone>> {
        if class == Class::In { &mut self.in_root }
        else { self.roots.entry(class).or_insert_with(|| Node::new(None)) }
    }
}


//--- Name Service

impl NameService for AuthoritativeZones {
    type Future = Done<Vec<u8>, io::Error>;

    fn call(&self, req: MessageBuf, mode: ComposeMode) -> Self::Future {
        let mut resp = MessageBuilder::new(mode, true).unwrap();
        resp.header_mut().set_id(req.header().id());
        resp.header_mut().set_qr(true);
        resp.header_mut().set_opcode(req.header().opcode());
        
        let _question = match req.question().next() {
            Some(Ok(question)) => question,
            Some(Err(_)) | None => {
                resp.header_mut().set_rcode(Rcode::FormErr);
                return done(Ok(resp.finish()))
            }
        };
        unimplemented!()
    }

    fn poll_ready(&self) -> Async<()> {
        Async::Ready(())
    }
}


//------------ Zone ----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Zone {
    data: Node<Option<ZoneEntry>>,
}

impl Zone {
    pub fn new() -> Self {
        Zone {
            data: Node::new(None),
        }
    }

    pub fn add_record<N: DName>(&mut self, name: &N, ttl: u32,
                                data: MasterRecordData) -> Result<(), ()> {
        let node = try!(self.build_node(name));
        if node.value().is_none() {
            *node.value_mut() = Some(Entry::Authoritative(Records::new()));
        }
        match *node.value_mut().as_mut().unwrap() {
            Entry::Authoritative(ref mut records) => {
                records.add_record(ttl, data)
            }
            Entry::Cut(..) => {
                Err(())
            }
        }
    }

    pub fn add_cut<N: DName>(&mut self, name: &N) -> Result<&mut Cut, ()> {
        let node = try!(self.build_node(name));
        if node.value().is_none() {
            *node.value_mut() = Some(Entry::Cut(Cut::new()));
        }
        match *node.value_mut().as_mut().unwrap() {
            Entry::Authoritative(..) => Err(()),
            Entry::Cut(ref mut cut) => Ok(cut)
        }
    }

    fn build_node<N: DName>(&mut self, name: &N)
                         -> Result<&mut Node<Option<ZoneEntry>>, ()> {
        // By wrapping node in an Option, we get around borrowchk’s
        // complains about mutable borrows and borrowed assignments.
        let mut node = Some(&mut self.data);
        for ltte in name.labelettes().rev() {
            let child = try!(node.unwrap().build_child(ltte, || Ok(None)));
            node = Some(child);
        }
        Ok(node.unwrap())
    }
}

impl Zone {
    pub fn query<'a, I>(&self, iter: I, rtype: Rtype)
                        -> Option<Entry<Option<&RRset<MasterRecordData>>,
                                        &Cut>>
                 where I: Iterator<Item=Labelette<'a>> {
        let mut node = &self.data;
        for ltte in iter {
            match node.get_child(ltte) {
                Some(child) => {
                    node = child
                }
                None => {
                    match node.get_child(Labelette::Normal(b"*")) {
                        Some(child) => {
                            node = child;
                            break;
                        }
                        None => return None
                    }
                }
            }
        }
        match *node.value() {
            Some(Entry::Authoritative(ref records)) => {
                Some(Entry::Authoritative(records.get(rtype)))
            }
            Some(Entry::Cut(ref cut)) => {
                Some(Entry::Cut(cut))
            }
            None => {
                Some(Entry::Authoritative(None))
            }
        }
    }
}


//------------ Entry ---------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Entry<A, C> {
    Authoritative(A),
    Cut(C)
}

type ZoneEntry = Entry<Records, Cut>;


//------------ Records -------------------------------------------------------

#[derive(Clone, Debug)]
struct Records {
    rrsets: HashMap<Rtype, RRset<MasterRecordData>>,
}

impl Records {
    pub fn new() -> Self {
        Records {
            rrsets: HashMap::new()
        }
    }

    pub fn add_record(&mut self, ttl: u32, data: MasterRecordData)
                      -> Result<(), ()> {
        let rrset = self.rrsets.entry(data.rtype())
                               .or_insert_with(RRset::new);
        if rrset.ttl() == 0 {
            rrset.set_ttl(ttl)
        }
        else if rrset.ttl() != ttl {
            return Err(())
        }
        rrset.push(data);
        Ok(())
    }
}


impl Records {
    pub fn get(&self, rtype: Rtype) -> Option<&RRset<MasterRecordData>> {
        self.rrsets.get(&rtype)
    }
}


//------------ RRset ---------------------------------------------------------

#[derive(Clone, Debug)]
pub struct RRset<D> {
    ttl: u32,
    data: Vec<D>
}


impl<D> RRset<D> {
    pub fn new() -> Self {
        RRset{ttl: 0, data: Vec::new()}
    }

    pub fn ttl(&self) -> u32 {
        self.ttl
    }

    pub fn set_ttl(&mut self, ttl: u32) {
        self.ttl = ttl
    }

    pub fn push(&mut self, data: D) {
        self.data.push(data)
    }
}

impl<D> Deref for RRset<D> {
    type Target = [D];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}


//------------ Cut -----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Cut {
    ns: RRset<Ns>,
    //ds: RRset<Ds>,
    glue: Vec<Record<DNameBuf, MasterRecordData>>
}

impl Cut {
    pub fn new() -> Self {
        Cut {
            ns: RRset::new(),
            glue: Vec::new(),
        }
    }

    pub fn ns(&self) -> &RRset<Ns> {
        &self.ns
    }

    pub fn glue(&self) -> &[Record<DNameBuf, MasterRecordData>] {
        &self.glue
    }
}


//------------ Node ----------------------------------------------------------

#[derive(Clone, Debug)]
struct Node<V> {
    value: V,
    children: NodeChildren<V>,
}

impl<V> Node<V> {
    pub fn new(value: V) -> Self {
        Node { 
            value: value, 
            children: NodeChildren::new()
        }
    }

    pub fn value(&self) -> &V {
        &self.value
    }

    pub fn value_mut(&mut self) -> &mut V {
        &mut self.value
    }

    pub fn get_child(&self, labelette: Labelette) -> Option<&Self> {
        self.children.get(labelette)
    }

    pub fn build_child<F, E>(&mut self, labelette: Labelette, insertop: F)
                             -> Result<&mut Self, E>
                       where F: Fn() -> Result<V, E> {
        self.children.build(labelette, || insertop())
    }

    pub fn build_node<'a, I, F, E>(&mut self, mut iter: I, insertop: F)
                                   -> Result<&mut Self, E>
                      where I: Iterator<Item=Labelette<'a>>
                               + DoubleEndedIterator,
                            F: Fn(&V) -> Result<V, E> {
        // Not sure this can be done iteratively due to all the mutable
        // references. Yes, this is a challenge.
        if let Some(labelette) = iter.next_back() {
            let (children, value) = (&mut self.children, &self.value);
            let child = try!(children.build(labelette, || insertop(value)));
            child.build_node(iter, insertop)
        }
        else {
            Ok(self)
        }
    }
}


//------------ NodeChildren --------------------------------------------------

#[derive(Debug)]
struct NodeChildren<V> {
    /// Children with normal labels.
    normal: HashMap<Vec<u8>, Node<V>>,

    /// Children for the binary labels. First is for false, second for true.
    binary: [Option<Box<Node<V>>>; 2],
}

impl<V> NodeChildren<V> {
    pub fn new() -> Self {
        NodeChildren {
            normal: HashMap::new(),
            binary: [None, None]
        }
    }

    pub fn get(&self, labelette: Labelette) -> Option<&Node<V>> {
        match labelette {
            Labelette::Normal(bytes) => self.normal.get(bytes),
            Labelette::Bit(bit) => {
                self.binary[bit_index(bit)].as_ref().map(|x| x.deref())
            }
        }
    }

    pub fn build<F, E>(&mut self, labelette: Labelette, insertop: F)
                       -> Result<&mut Node<V>, E>
                 where F: Fn() -> Result<V, E> {
        match labelette {
            Labelette::Normal(bytes) => {
                // The use of contains_key() here is because borrowck won’t
                // let us use both get_mut() and entry() in the same scope.
                // Sadly, we can’t use entry() right away either because it
                // needs an owned key.
                if self.normal.contains_key(bytes) {
                    Ok(self.normal.get_mut(bytes).unwrap())
                }
                else {
                    insertop().map(move |value| {
                        self.normal.entry(bytes.into())
                                   .or_insert(Node::new(value))
                    })
                }
            }
            Labelette::Bit(bit) => {
                if self.binary[bit_index(bit)].is_none() {
                    let value = try!(insertop());
                    self.binary[bit_index(bit)]
                        = Some(Box::new(Node::new(value)));
                }
                Ok(self.binary[bit_index(bit)].as_mut().unwrap().deref_mut())
            }
        }
    }
}

impl<V: Clone> Clone for NodeChildren<V> {
    fn clone(&self) -> Self {
        NodeChildren {
            normal: self.normal.clone(),
            binary: [self.binary[0].clone(), self.binary[1].clone()],
        }
    }
}

fn bit_index(bit: bool) -> usize {
    if bit { 1 } else { 0 }
}


//============ Tests =========================================================

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use ::bits::{DName, DNameBuf, Question, RecordData};
    use ::iana::{Class, Rtype};
    use ::rdata::MasterRecordData;
    use ::rdata::owned::{A, Aaaa, Soa, Txt};
    use super::*;

    fn add_zone<'a>(zones: &'a mut AuthoritativeZones, name: &str,
                    records: Vec<(&str, MasterRecordData)>) {
        let zone = zones.add_zone(
            Soa::record(
                DNameBuf::from_str(name).unwrap(),
                Class::In, 3600,
                DNameBuf::from_str("ns.example.com.").unwrap(),
                DNameBuf::from_str("hostmaster.example.com.").unwrap(),
                1, 86400, 7200, 3600000, 172800
            )
        ).unwrap();
        for (name, data) in records {
            zone.add_record(DNameBuf::from_str(name).unwrap()
                                                    .rev_labelettes(),
                            data.rtype(), 3600, data).unwrap()
        }
    }

    fn query_auth<'a>(zones: &'a AuthoritativeZones, name: &str,
                      qtype: Rtype) -> &'a RRset<MasterRecordData> {
        let question = Question::new(DNameBuf::from_str(name).unwrap(),
                                     qtype, Class::In);
        match zones.query(&question).unwrap() {
            Entry::Authoritative(x) => x.unwrap(),
            _ => panic!("not an authoritative entry")
        }
    }

    #[test]
    fn test() {
        let mut zones = AuthoritativeZones::new();
        add_zone(&mut zones, "one.one.example.com.",
                 vec![("", MasterRecordData::A(A::from_octets(127,0,0,1))),
                      ("www", MasterRecordData::A(A::from_octets(127,0,0,2)))]);
        add_zone(&mut zones, "two.one.example.com.", vec![]);
        add_zone(&mut zones, "three.example.com.", vec![]);

        match *query_auth(&zones, "www.one.one.example.com.", Rtype::A)
                .first().unwrap() {
            MasterRecordData::A(ref a) => {
                assert_eq!(a.addr(), ::std::net::Ipv4Addr::from([127,0,0,2]))
            }
            _ => panic!("wrong record type")
        }
    }
}

