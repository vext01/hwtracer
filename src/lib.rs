#![cfg_attr(feature = "clippy", feature(plugin))]
#![cfg_attr(feature = "clippy", plugin(clippy))]
#![feature(optin_builtin_traits)]
#![feature(link_args)]

pub mod backends;
pub mod errors;

pub use errors::HWTracerError;
use std::fmt::Debug;
use std::fmt::{self, Display, Formatter};
#[cfg(test)]
use std::fs::File;
use std::iter::Iterator;

/// Information about a basic block.
#[derive(Debug, Eq, PartialEq)]
pub struct Block {
    /// Virtual address of the first instruction in this block.
    first_instr: u64,
    /// Virtual address of the last instruction in this block.
    last_instr: u64,
}

impl Block {
    /// Creates a new basic block from a start address and a length in bytes.
    pub fn new(first_instr: u64, last_instr: u64) -> Self {
        Self {
            first_instr,
            last_instr,
        }
    }

    /// Returns the virtual address of the first instruction in this block.
    pub fn first_instr(&self) -> u64 {
        self.first_instr
    }

    /// Returns the virtual address of the last instruction in this block.
    pub fn last_instr(&self) -> u64 {
        self.last_instr
    }
}

/// Represents a generic trace.
///
/// Each backend has its own concrete implementation.
pub trait Trace: Debug + Send {
    /// Dump the trace to the specified filename.
    ///
    /// The exact format varies per-backend.
    #[cfg(test)]
    fn to_file(&self, file: &mut File);

    /// Iterate over the blocks of the trace.
    fn iter_blocks<'t: 'i, 'i>(
        &'t self,
    ) -> Box<dyn Iterator<Item = Result<Block, HWTracerError>> + 'i>;

    /// Get the capacity of the trace in bytes.
    #[cfg(test)]
    fn capacity(&self) -> usize;
}

/// The interface offered by all tracer types.
pub trait Tracer: Send + Sync {
    /// Return a `ThreadTracer` for tracing the current thread.
    fn thread_tracer(&self) -> Box<dyn ThreadTracer>;
}

pub trait ThreadTracer {
    /// Start recording a trace.
    ///
    /// Tracing continues until [stop_tracing](trait.ThreadTracer.html#method.stop_tracing) is called.
    fn start_tracing(&mut self) -> Result<(), HWTracerError>;
    /// Turns off the tracer.
    ///
    /// [start_tracing](trait.ThreadTracer.html#method.start_tracing) must have been called prior.
    fn stop_tracing(&mut self) -> Result<Box<dyn Trace>, HWTracerError>;
}

// Keeps track of the internal state of a tracer.
#[derive(PartialEq, Eq, Debug)]
pub enum TracerState {
    Stopped,
    Started,
}

impl TracerState {
    /// Returns the error corresponding with the `TracerState`.
    pub fn as_error(self) -> HWTracerError {
        HWTracerError::TracerState(self)
    }
}

impl Display for TracerState {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match *self {
            TracerState::Started => write!(f, "started"),
            TracerState::Stopped => write!(f, "stopped"),
        }
    }
}

// Test helpers.
//
// Each struct implementing the [ThreadTracer](trait.ThreadTracer.html) trait should include tests
// calling the following helpers.
#[cfg(test)]
mod test_helpers {
    use super::{Block, HWTracerError, ThreadTracer, TracerState};
    use crate::Trace;
    use std::slice::Iter;
    use std::time::SystemTime;

    // A loop that does some work that we can use to build a trace.
    #[inline(never)]
    pub fn work_loop(iters: u64) -> u64 {
        let mut res = 0;
        for _ in 0..iters {
            // Computation which stops the compiler from eliminating the loop.
            res += SystemTime::now().elapsed().unwrap().subsec_nanos() as u64;
        }
        res
    }

    // Trace a closure that returns a u64.
    pub fn trace_closure<F>(tracer: &mut dyn ThreadTracer, f: F) -> Box<dyn Trace>
    where
        F: FnOnce() -> u64,
    {
        tracer.start_tracing().unwrap();
        let res = f();
        let trace = tracer.stop_tracing().unwrap();
        println!("traced closure with result: {}", res); // To avoid over-optimisation.
        trace
    }

    // Check that starting and stopping a tracer works.
    pub fn test_basic_usage<T>(mut tracer: T)
    where
        T: ThreadTracer,
    {
        trace_closure(&mut tracer, || work_loop(500));
    }

    // Check that repeated usage of the same tracer works.
    pub fn test_repeated_tracing<T>(mut tracer: T)
    where
        T: ThreadTracer,
    {
        for _ in 0..10 {
            trace_closure(&mut tracer, || work_loop(500));
        }
    }

    // Check that starting a tracer twice makes an appropriate error.
    pub fn test_already_started<T>(mut tracer: T)
    where
        T: ThreadTracer,
    {
        tracer.start_tracing().unwrap();
        match tracer.start_tracing() {
            Err(HWTracerError::TracerState(TracerState::Started)) => (),
            _ => panic!(),
        };
        tracer.stop_tracing().unwrap();
    }

    // Check that stopping an unstarted tracer makes an appropriate error.
    pub fn test_not_started<T>(mut tracer: T)
    where
        T: ThreadTracer,
    {
        match tracer.stop_tracing() {
            Err(HWTracerError::TracerState(TracerState::Stopped)) => (),
            _ => panic!(),
        };
    }

    // Helper to check an expected list of blocks matches what we actually got.
    pub fn test_expected_blocks(trace: Box<dyn Trace>, mut expect_iter: Iter<Block>) {
        let mut got_iter = trace.iter_blocks();
        loop {
            let expect = expect_iter.next();
            let got = got_iter.next();
            if expect.is_none() || got.is_none() {
                break;
            }
            assert_eq!(
                got.unwrap().unwrap().first_instr(),
                expect.unwrap().first_instr()
            );
        }
        // Check that both iterators were the same length.
        assert!(expect_iter.next().is_none());
        assert!(got_iter.next().is_none());
    }

    // Trace two loops, one 10x larger than the other, then check the proportions match the number
    // of block the trace passes through.
    #[cfg(perf_pt_test)]
    pub fn test_ten_times_as_many_blocks<T>(mut tracer1: T, mut tracer2: T)
    where
        T: ThreadTracer,
    {
        let trace1 = trace_closure(&mut tracer1, || work_loop(10));
        let trace2 = trace_closure(&mut tracer2, || work_loop(100));

        // Should be roughly 10x more blocks in trace2. It won't be exactly 10x, due to the stuff
        // we trace either side of the loop itself. On a smallish trace, that will be significant.
        let (ct1, ct2) = (trace1.iter_blocks().count(), trace2.iter_blocks().count());
        assert!(ct2 > ct1 * 9);
    }
}
