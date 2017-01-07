
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use ::bits::name::DNameBuf;
use ::iana::Class;
use ::master::bufscanner::BufScanner;
use ::master::entry::Entry;
use ::master::error::{ScanError, ScanResult};
use ::master::record::MasterRecord;
use ::master::scanner::Scanner;


//------------ Reader --------------------------------------------------------

pub struct Reader<S: Scanner> {
    scanner: Option<S>,
    origin: Option<Rc<DNameBuf>>,
    ttl: Option<u32>,
    last: Option<(Rc<DNameBuf>, Class)>,
}

impl<S: Scanner> Reader<S> {
    pub fn new(scanner: S) -> Self {
        Reader {
            scanner: Some(scanner),
            origin: None,
            ttl: None,
            last: None
        }
    }

    pub fn set_origin(&mut self, origin: Option<Rc<DNameBuf>>) {
        self.origin = origin
    }
}

impl Reader<BufScanner<File>> {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Reader::new(try!(BufScanner::open(path))))
    }
}

impl<T: AsRef<[u8]>> Reader<BufScanner<io::Cursor<T>>> {
    pub fn create(t: T) -> Self {
        Reader::new(BufScanner::create(t))
    }
}

impl<S: Scanner> Reader<S> {
    fn last_owner(&self) -> Option<Rc<DNameBuf>> {
        if let Some((ref name, _)) = self.last {
            Some(name.clone())
        }
        else {
            None
        }
    }

    fn last_class(&self) -> Option<Class> {
        if let Some((_, class)) = self.last {
            Some(class)
        }
        else {
            None
        }
    }

    fn next_entry(&mut self) -> ScanResult<Option<Entry>> {
        let last_owner = self.last_owner();
        let last_class = self.last_class();
        if let Some(ref mut scanner) = self.scanner {
            Entry::scan(scanner, last_owner, last_class, &self.origin,
                        self.ttl)
        }
        else {
            Ok(None)
        }
    }

    #[allow(match_same_arms)]
    pub fn next_record(&mut self) -> ScanResult<Option<ReaderItem>> {
        loop {
            match self.next_entry() {
                Ok(Some(Entry::Origin(origin))) => self.origin = Some(origin),
                Ok(Some(Entry::Include{path, origin})) => {
                    return Ok(Some(ReaderItem::Include { path: path,
                                                         origin: origin }))
                }
                Ok(Some(Entry::Ttl(ttl))) => self.ttl = Some(ttl),
                Ok(Some(Entry::Control{..})) => { },
                Ok(Some(Entry::Record(record))) => {
                    self.last = Some((record.owner.clone(), record.class));
                    return Ok(Some(ReaderItem::Record(record)))
                }
                Ok(Some(Entry::Blank)) => { }
                Ok(None) => return Ok(None),
                Err(err) => {
                    self.scanner = None;
                    return Err(err)
                }
            }
        }
     }
}

impl<S: Scanner> Iterator for Reader<S> {
    type Item = ScanResult<ReaderItem>;

    fn next(&mut self) -> Option<ScanResult<ReaderItem>> {
        match self.next_record() {
            Ok(Some(res)) => Some(Ok(res)),
            Ok(None) => None,
            Err(err) => Some(Err(err))
        }
    }
}


//------------ FileReader ----------------------------------------------------

pub type FileReader = Reader<BufScanner<File>>;


//------------ ReaderItem ----------------------------------------------------

#[derive(Clone, Debug)]
pub enum ReaderItem {
    Record(MasterRecord),
    Include { path: Vec<u8>, origin: Option<Rc<DNameBuf>> }
}

impl fmt::Display for ReaderItem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ReaderItem::Record(ref record) => write!(f, "{}", record),
            ReaderItem::Include{ref path, ref origin} => {
                try!(write!(f, "$INCLUDE {}", String::from_utf8_lossy(path)));
                if let Some(ref origin) = *origin {
                    try!(write!(f, " {}", origin));
                }
                Ok(())
            }
        }
    }
}


//------------ FileReaderIter ------------------------------------------------

pub struct FileReaderIter {
    /// The stack of files we are working on.
    ///
    /// We need this because of includes. The first element is file name.
    stack: Vec<(PathBuf, FileReader)>,
}

impl FileReaderIter {
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref();
        FileReader::open(path).map(|file| {
            FileReaderIter{stack: vec![(path.into(), file)]}
        })
    }
}

impl Iterator for FileReaderIter {
    type Item = Result<MasterRecord, FileReaderError>;

    fn next(&mut self) -> Option<Self::Item> {
        // XXX This currently breaks at the first error encountered. To do
        //     this properly, we need to make the scanner more resilient.
        loop {
            let more = {
                let (name, reader) = match self.stack.last_mut() {
                    Some(item) => (&item.0, &mut item.1),
                    None => return None
                };
                match reader.next_record() {
                    Ok(Some(ReaderItem::Record(record))) => {
                        return Some(Ok(record))
                    }
                    Ok(Some(ReaderItem::Include{path, origin})) => {
                        // XXX We assume UTF8 for path for now. This will
                        //     be fixed when the scanner is switched from u8
                        //     to char (#6). Because of this, we won’t bother
                        //     with proper error messages either.
                        match ::std::str::from_utf8(&path) {
                            Ok(path) => {
                                // Unwrap here is fine: If name were empty,
                                // how could we have an open file?
                                let dir = name.parent().unwrap();
                                Ok(Some((dir.join(path), origin)))
                            }
                            Err(_) => {
                                Err(FileReaderError::new_other(name.clone(),
                                               "Illegal include file name"))
                            }
                        }
                    }
                    Ok(None) => {
                        Ok(None)
                    }
                    Err(err) => {
                        Err(FileReaderError::new(name.clone(), err))
                    }
                }
            };
            match more {
                Ok(Some((path, origin))) => {
                    match FileReader::open(&path) {
                        Ok(mut reader) => {
                            reader.set_origin(origin);
                            self.stack.push((path, reader))
                        }
                        Err(err) => {
                            self.stack.clear();
                            return Some(Err(FileReaderError::new(path, err)))
                        }
                    }
                }
                Ok(None) => {
                    self.stack.pop().unwrap();
                }
                Err(err) => {
                    self.stack.clear();
                    return Some(Err(err))
                }
            }
        }
    }
}


//------------ FileReaderError -----------------------------------------------

pub struct FileReaderError {
    path: PathBuf,
    error: ScanError,
}

impl FileReaderError {
    fn new<E: Into<ScanError>>(path: PathBuf, error: E) -> Self {
        FileReaderError{path: path, error: error.into()}
    }

    fn new_other<E>(path: PathBuf, error: E) -> Self
                 where E: Into<Box<::std::error::Error + Send + Sync>> {
        FileReaderError::new(path,
                             io::Error::new(io::ErrorKind::Other, error))
    }
}

impl FileReaderError {
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn error(&self) -> &ScanError {
        &self.error
    }
}


//============ Test ==========================================================

#[cfg(test)]
mod test {
    use super::*;
    use ::master::error::ScanError;

    #[test]
    fn print() {
        let reader = Reader::create(&b"$ORIGIN ISI.EDU.
$TTL 86400
@   IN  SOA     VENERA      Action\\.domains (
                                 20     ; SERIAL
                                 7200   ; REFRESH
                                 600    ; RETRY
                                 3600000; EXPIRE
                                 60)    ; MINIMUM

        NS      A.ISI.EDU.
        NS      VENERA
        NS      VAXA
        MX      10      VENERA
        MX      20      VAXA
   
A       A       26.3.0.103

VENERA  A       10.1.0.52
        A       128.9.0.32

VAXA    A       10.2.0.27
        A       128.9.0.33


$INCLUDE <SUBSYS>ISI-MAILBOXES.TXT"[..]);

        for item in reader {
            match item {
                Ok(item) => println!("{}", item),
                Err(ScanError::Syntax(err, pos)) => {
                    println!("{}:{}:  {:?}", pos.line(), pos.col(), err);
                }
                Err(err) => println!("{:?}", err)
            }
        }
    }
}

