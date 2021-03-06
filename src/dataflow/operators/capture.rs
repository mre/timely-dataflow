//! An operator which captures the streams of records and notifications to play back later.
//!
//! Not yet tested; please be careful using!

use std::rc::Rc;
use std::cell::RefCell;

use std::io::{Read, Write};
use std::ops::DerefMut;

use ::Data;
use dataflow::{Scope, Stream};
use dataflow::channels::pact::{ParallelizationContract, Pipeline};
use dataflow::channels::pullers::Counter as PullCounter;
use dataflow::channels::pushers::Counter as PushCounter;
use dataflow::channels::pushers::buffer::Buffer as PushBuffer;
use dataflow::channels::pushers::Tee;

use progress::count_map::CountMap;
use progress::nested::subgraph::{Source, Target};
use progress::{Timestamp, Operate, Antichain};

use abomonation::Abomonation;

/// Capture a stream of timestamped data for later replay.
pub trait Capture<T: Timestamp, D: Data> {
    /// Captures a stream of timestamped data for later replay.
    ///
    /// #Examples
    ///
    /// The type `Rc<EventLink<T,D>>` implements a typed linked list,
    /// and can be captured into and replayed from.
    ///
    /// ```
    /// use std::rc::Rc;
    /// use timely::dataflow::Scope;
    /// use timely::dataflow::operators::{Capture, ToStream, Inspect};
    /// use timely::dataflow::operators::capture::{EventLink, Replay};
    ///
    /// timely::execute(timely::Configuration::Thread, |computation| {
    ///     let handle1 = Rc::new(EventLink::new());
    ///     let handle2 = handle1.clone();
    ///
    ///     computation.scoped::<u64,_,_>(|scope1|
    ///         (0..10).to_stream(scope1)
    ///                .capture_into(handle1)
    ///     );
    ///
    ///     computation.scoped(|scope2| {
    ///         handle2.replay_into(scope2)
    ///                .inspect(|x| println!("replayed: {:?}", x));
    ///     })
    /// });
    /// ```
    ///
    /// The types `EventWriter<T, D, W>` and `EventReader<T, D, R>` can be
    /// captured into and replayed from, respectively. The use binary writers
    /// and readers respectively, and can be backed by files, network sockets,
    /// etc.
    ///
    /// ```
    /// use std::rc::Rc;
    /// use std::net::{TcpListener, TcpStream};
    /// use timely::dataflow::Scope;
    /// use timely::dataflow::operators::{Capture, ToStream, Inspect};
    /// use timely::dataflow::operators::capture::{EventReader, EventWriter, Replay};
    ///
    /// timely::execute(timely::Configuration::Thread, |computation| {
    ///     let list = TcpListener::bind("127.0.0.1:8000").unwrap();
    ///     let send = TcpStream::connect("127.0.0.1:8000").unwrap();
    ///     let recv = list.incoming().next().unwrap().unwrap();
    ///
    ///     computation.scoped::<u64,_,_>(|scope1|
    ///         (0..10u64)
    ///             .to_stream(scope1)
    ///             .capture_into(EventWriter::new(send))
    ///     );
    ///
    ///     computation.scoped::<u64,_,_>(|scope2| {
    ///         EventReader::<_,u64,_>::new(recv)
    ///             .replay_into(scope2)
    ///             .inspect(|x| println!("replayed: {:?}", x));
    ///     })
    /// });
    /// ```
    fn capture_into<P: EventPusher<T, D>+'static>(&self, pusher: P);
}

impl<S: Scope, D: Data> Capture<S::Timestamp, D> for Stream<S, D> {
    fn capture_into<P: EventPusher<S::Timestamp, D>+'static>(&self, pusher: P) {

        let mut scope = self.scope();   // clones the scope
        let channel_id = scope.new_identifier();

        let (sender, receiver) = Pipeline.connect(&mut scope, channel_id);
        let operator = CaptureOperator {
            input: PullCounter::new(receiver),
            events: pusher,
        };

        let index = scope.add_operator(operator);
        self.connect_to(Target { index: index, port: 0 }, sender, channel_id);
    }
}

/// Possible events that the captured stream may provide.
pub enum Event<T, D> {
    Start,
    /// Progress received via `push_external_progress`.
    Progress(Vec<(T, i64)>),
    /// Messages received via the data stream.
    Messages(T, Vec<D>),
}


impl<T: Abomonation, D: Abomonation> Abomonation for Event<T,D> {
    #[inline] unsafe fn embalm(&mut self) {
        if let &mut Event::Progress(ref mut vec) = self { vec.embalm(); }
        if let &mut Event::Messages(ref mut time, ref mut data) = self { time.embalm(); data.embalm(); }
    }
    #[inline] unsafe fn entomb(&self, bytes: &mut Vec<u8>) {
        if let &Event::Progress(ref vec) = self { vec.entomb(bytes); }
        if let &Event::Messages(ref time, ref data) = self { time.entomb(bytes); data.entomb(bytes); }
    }
    #[inline] unsafe fn exhume<'a, 'b>(&'a mut self, mut bytes: &'b mut[u8]) -> Option<&'b mut [u8]> {
        match self {
            &mut Event::Progress(ref mut vec) => { vec.exhume(bytes) },
            &mut Event::Messages(ref mut time, ref mut data) => {
                let temp = bytes; bytes = if let Some(bytes) = time.exhume(temp) { bytes } else { return None; };
                let temp = bytes; bytes = if let Some(bytes) = data.exhume(temp) { bytes } else { return None; };
                Some(bytes)
            }
            &mut Event::Start => Some(bytes),
        }
    }
}


/// Linked list of events.
pub struct EventLink<T, D> {
    /// An event.
    pub event: Event<T, D>,
    /// The next event, if it exists.
    pub next: RefCell<Option<Rc<EventLink<T, D>>>>,
}

impl<T, D> EventLink<T, D> { pub fn new() -> EventLink<T, D> { EventLink { event: Event::Start, next: RefCell::new(None) }}}

pub trait EventIterator<T, D> {
    fn next<'a>(&'a mut self) -> Option<&'a Event<T, D>>;
}

pub trait EventPusher<T, D> {
    fn push(&mut self, event: Event<T, D>);
}



// implementation for the linked list behind a `Handle`.
impl<T, D> EventPusher<T, D> for Rc<EventLink<T, D>> {
    fn push(&mut self, event: Event<T, D>) {
        *self.next.borrow_mut() = Some(Rc::new(EventLink { event: event, next: RefCell::new(None) }));
        let next = self.next.borrow().as_ref().unwrap().clone();
        *self = next;
    }
}

impl<T, D> EventIterator<T, D> for Rc<EventLink<T, D>> {
    fn next<'a>(&'a mut self) -> Option<&'a Event<T, D>> {
        let is_some = self.next.borrow().is_some();
        if is_some {
            let next = self.next.borrow().as_ref().unwrap().clone();
            *self = next;
            Some(&self.event)
        }
        else {
            None
        }
    }
}


pub struct EventWriter<T, D, W: ::std::io::Write> {
    buffer: Vec<u8>,
    stream: W,
    phant: ::std::marker::PhantomData<(T,D)>,
}

impl<T, D, W: ::std::io::Write> EventWriter<T, D, W> {
    pub fn new(w: W) -> EventWriter<T, D, W> {
        EventWriter {
            buffer: vec![],
            stream: w,
            phant: ::std::marker::PhantomData,
        }
    }
}

impl<T: Abomonation, D: Abomonation, W: ::std::io::Write> EventPusher<T, D> for EventWriter<T, D, W> {
    fn push(&mut self, event: Event<T, D>) {
        unsafe { ::abomonation::encode(&event, &mut self.buffer); }
        self.stream.write_all(&self.buffer[..]).unwrap();
        self.buffer.clear();
    }
}


pub struct EventReader<T, D, R: ::std::io::Read> {
    reader: R,
    bytes: Vec<u8>,
    buff1: Vec<u8>,
    buff2: Vec<u8>,
    consumed: usize,
    valid: usize,
    phant: ::std::marker::PhantomData<(T,D)>,
}

impl<T, D, R: ::std::io::Read> EventReader<T, D, R> {
    pub fn new(r: R) -> EventReader<T, D, R> {
        EventReader {
            reader: r,
            bytes: vec![0u8; 1 << 20],
            buff1: vec![],
            buff2: vec![],
            consumed: 0,
            valid: 0,
            phant: ::std::marker::PhantomData,
        }
    }
}

impl<T: Abomonation, D: Abomonation, R: ::std::io::Read> EventIterator<T, D> for EventReader<T, D, R> {
    fn next<'a>(&'a mut self) -> Option<&'a Event<T, D>> {

        // if we can decode something, we should just return it! :D
        if unsafe { ::abomonation::decode::<Event<T,D>>(&mut self.buff1[self.consumed..]) }.is_some() {
            let (item, rest) = unsafe { ::abomonation::decode::<Event<T,D>>(&mut self.buff1[self.consumed..]) }.unwrap();
            self.consumed = self.valid - rest.len();
            return Some(item);
        }
        else {
            // if we exhaust data we should shift back (if any shifting to do)
            if self.consumed > 0 {
                self.buff2.clear();
                self.buff2.write_all(&mut self.buff1[self.consumed..]).unwrap();
                ::std::mem::swap(&mut self.buff1, &mut self.buff2);
                self.valid = self.buff1.len();
                self.consumed = 0;
            }

            if let Ok(len) = self.reader.read(&mut self.bytes[..]) {
                self.buff1.write_all(&self.bytes[..len]).unwrap();
                self.valid = self.buff1.len();
            }

            return None;
        }
    }
}

pub trait Replay<T: Timestamp, D: Data> {
    fn replay_into<S: Scope<Timestamp=T>>(self, scope: &mut S) -> Stream<S, D>;
}

impl<T: Timestamp, D: Data, I: EventIterator<T, D>+'static> Replay<T, D> for I {
    fn replay_into<S: Scope<Timestamp=T>>(self, scope: &mut S) -> Stream<S, D>{
       let (targets, registrar) = Tee::<S::Timestamp, D>::new();
       let operator = ReplayOperator {
           output: PushBuffer::new(PushCounter::new(targets, Rc::new(RefCell::new(CountMap::new())))),
           events: self,
       };

       let index = scope.add_operator(operator);
       Stream::new(Source { index: index, port: 0 }, registrar, scope.clone())
   }
}

struct CaptureOperator<T: Timestamp, D: Data, P: EventPusher<T, D>> {
    input: PullCounter<T, D>,
    events: P,
}

impl<T:Timestamp, D: Data, P: EventPusher<T, D>> Operate<T> for CaptureOperator<T, D, P> {
    fn name(&self) -> String { "Capture".to_owned() }
    fn inputs(&self) -> usize { 1 }
    fn outputs(&self) -> usize { 0 }

    // we need to set the initial value of the frontier
    fn set_external_summary(&mut self, _: Vec<Vec<Antichain<T::Summary>>>, counts: &mut [CountMap<T>]) {
        self.events.push(Event::Progress(counts[0].clone().into_inner()));
        counts[0].clear();
    }

    // each change to the frontier should be shared
    fn push_external_progress(&mut self, counts: &mut [CountMap<T>]) {
        self.events.push(Event::Progress(counts[0].clone().into_inner()));
        counts[0].clear();
    }

    fn pull_internal_progress(&mut self, consumed: &mut [CountMap<T>],  _: &mut [CountMap<T>], _: &mut [CountMap<T>]) -> bool {
        while let Some((time, data)) = self.input.next() {
            self.events.push(Event::Messages(time.clone(), data.deref_mut().clone()));
        }
        self.input.pull_progress(&mut consumed[0]);
        false
    }
}

struct ReplayOperator<T:Timestamp, D: Data, I: EventIterator<T, D>> {
    events: I,
    output: PushBuffer<T, D, PushCounter<T, D, Tee<T, D>>>,
}

impl<T:Timestamp, D: Data, I: EventIterator<T, D>> Operate<T> for ReplayOperator<T, D, I> {
    fn name(&self) -> String { "Replay".to_owned() }
    fn inputs(&self) -> usize { 0 }
    fn outputs(&self) -> usize { 1 }

    fn get_internal_summary(&mut self) -> (Vec<Vec<Antichain<T::Summary>>>, Vec<CountMap<T>>) {

        // panics if the event link has not been initialized; should only happen if no one has
        // called set_external_summary on the `CaptureOperator`. So, please don't use this in the
        // same graph as the `CaptureOperator`.

        // TODO : use Default::default() as the initial time, and in the first Event we dequeue,
        // TODO : announce that we've moved beyond Default::default().

        loop {
            let event = self.events.next();
            if let Some(event) = event {
                let mut result = CountMap::new();
                if let &Event::Progress(ref vec) = event {
                    for &(ref time, delta) in vec {
                        result.update(time, delta);
                    }
                }
                return (vec![], vec![result]);
            }
        }
        // if let Some(event) = self.events.next() {
        //     if let &Event::Progress(ref vec) = event {
        //         for &(ref time, delta) in vec {
        //             result.update(time, delta);
        //         }
        //     }
        // }
        // else {
        //     panic!("uninitialized replay; possibly in same computation as capture?");
        // }
    }

    fn pull_internal_progress(&mut self, _: &mut [CountMap<T>], internal: &mut [CountMap<T>], produced: &mut [CountMap<T>]) -> bool {

        while let Some(event) = self.events.next() {
            match *event {
                Event::Start => { },
                Event::Progress(ref vec) => {
                    for &(ref time, delta) in vec {
                        internal[0].update(time, delta);
                    }
                },
                Event::Messages(ref time, ref data) => {
                    let mut session = self.output.session(time);
                    for datum in data {
                        session.give(datum.clone());
                    }
                }
            }
        }

        self.output.cease();
        self.output.inner().pull_progress(&mut produced[0]);

        false
    }
}


#[cfg(test)]
mod tests {

    use ::Configuration;
    use dataflow::*;
    use dataflow::operators::{Capture, ToStream, Inspect};
    use super::{EventLink, Replay, EventWriter, EventReader};
    use std::rc::Rc;

    use std::net::{TcpListener, TcpStream};

    #[test]
    fn handle() {

        // initializes and runs a timely dataflow computation
        ::execute(Configuration::Thread, |computation| {
            let handle1 = Rc::new(EventLink::new());
            let handle2 = handle1.clone();
            computation.scoped::<u64,_,_>(|builder|
                (0..10).to_stream(builder)
                       .capture_into(handle1)
            );

            computation.scoped(|builder| {
                handle2.replay_into(builder)
                       .inspect(|x| println!("replayed: {:?}", x));
            })
        });
    }


    #[test]
    fn stream() {

        // initializes and runs a timely dataflow computation
        ::execute(Configuration::Thread, |computation| {

            let list = TcpListener::bind("127.0.0.1:8000").unwrap();
            let send = TcpStream::connect("127.0.0.1:8000").unwrap();
            let recv = list.incoming().next().unwrap().unwrap();

            computation.scoped::<u64,_,_>(|scope1|
                (0..10u64)
                    .to_stream(scope1)
                    .capture_into(EventWriter::new(send))
            );

            computation.scoped::<u64,_,_>(|scope2| {
                EventReader::<_,u64,_>::new(recv)
                    .replay_into(scope2)
                    .inspect(|x| println!("replayed: {:?}", x));
            })
        });
    }

}
