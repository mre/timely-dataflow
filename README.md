# Timely Dataflow #

Timely dataflow is a low-latency cyclic dataflow computational model, introduced in the paper [Naiad: a timely dataflow system](http://research.microsoft.com/pubs/201100/naiad_sosp2013.pdf).
This project is an extended and more modular implementation of timely dataflow in Rust.

Be sure to read the [documentation for timely dataflow](http://frankmcsherry.github.io/timely-dataflow). It is a work in progress, but mostly improving.

The [timely dataflow wiki](https://github.com/frankmcsherry/timely-dataflow/wiki) has more long-form text, introducing programming and explaining concepts in more detail.

# An example

To use timely dataflow, add the following to the dependencies section of your project's `Cargo.toml` file:

```
[dependencies]
timely="*"
```

This will bring in the [`timely` crate](https://crates.io/crates/timely) from [crates.io](http://crates.io), which should allow you to start writing timely dataflow programs like this one (also available in [examples/hello.rs](https://github.com/frankmcsherry/timely-dataflow/blob/master/examples/hello.rs)):

```rust
extern crate timely;

use timely::dataflow::*;
use timely::dataflow::operators::{Input, Inspect};

fn main() {
    // initializes and runs a timely dataflow computation
    timely::execute_from_args(std::env::args(), |root| {
        // create a new input and inspect its output
        let mut input = root.scoped(move |scope| {
            let (input, stream) = scope.new_input();
            stream.inspect(|x| println!("hello {}", x));
            input
        });

        // introduce data and watch!
        for round in 0..10 {
            input.send(format!("world: {}", round));
            input.advance_to(round + 1);
            root.step();
        }
    });
}
```

You can run this example from the root directory of the `timely-dataflow` repository by typing

```
% cargo run --example hello
Running `target/debug/examples/hello`
hello 0
hello 1
hello 2
hello 3
hello 4
hello 5
hello 6
hello 7
hello 8
hello 9
```

# Execution

The program above will by default use a single worker thread. To use multiple threads in a process, use the `-w` or `--workers` options followed by the number of threads you would like to use.

To use multiple processes, you will need to use the `-h` or `--hostfile` option to specify a text file whose lines are `hostname:port` entries corresponding to the locations you plan on spawning the processes. You will need to use the `-n` or `--processes` argument to indicate how many processes you will spawn (a prefix of the host file), and each process must use the `-p` or `--process` argument to indicate their index out of this number.

Said differently, you want a hostfile that looks like so,
```
% cat hostfile.txt
host0:port
host1:port
host2:port
host3:port
...
```
and then to launch the processes like so:
```
host0% cargo run -- -w 2 -h hostfile.txt -n 4 -p 0
host1% cargo run -- -w 2 -h hostfile.txt -n 4 -p 1
host2% cargo run -- -w 2 -h hostfile.txt -n 4 -p 2
host3% cargo run -- -w 2 -h hostfile.txt -n 4 -p 3
```
The number of workers should be the same for each process.

# The ecosystem

Timely dataflow is intended to support multiple levels of abstraction, from the lowest level manual dataflow assembly, to higher level "declarative" abstractions.

There are currently a few options for writing timely dataflow programs. Ideally this set will expand with time, as interested people write their own layers (or build on those of others).

* [**Timely dataflow**](https://github.com/frankmcsherry/timely-dataflow/tree/master/src/example_shared/operators): Timely dataflow includes several primitive operators, including standard operators like `map`, `filter`, and `concat`. It also including more exotic operators for tasks like entering and exiting loops (`enter` and `leave`), as well as generic operators whose implementations can be supplied using closures (`unary` and `binary`).

* [**Differential dataflow**](https://github.com/frankmcsherry/differential-dataflow): A higher-level language built on timely dataflow, differential dataflow includes operators like `group`, `join`, and `iterate`. Its implementation is fully incrementalized, and the details are pretty cool (if mysterious).

There are also a few application built on timely dataflow, including [a streaming worst-case optimal join implementation](https://github.com/frankmcsherry/dataflow_join) and a [PageRank](https://github.com/frankmcsherry/pagerank) implementation, both of which should provide helpful examples of writing timely dataflow programs.


# To-do list

There are several outstanding bits of work, each with the potential to sort out various performance issues.

## Rate-controlling output

At the moment, the implementations of `unary` and `binary` operators allow their closures to send un-bounded amounts of output. This can cause unwelcome resource exhaustion, and poor performance generally if the runtime needs to allocate lots of new memory to buffer data sent in bulk without being given a chance to digest it. It is commonly the case that when large amounts of data are produced, they are eventually reduced given the opportunity.

With the current interfaces there is not much to be done. One possible change would be to have the `input` and `notificator` objects ask for a closure from an input message or timestamp, respectively, to an output iterator. This gives the system the chance to play the iterator at the speed they feel is appropriate. As many operators produce data-parallel output (based on independent keys), it may not be that much of a burden to construct such iterators.

## Buffer management

The timely communication layer currently discards most buffers it moves through exchange channels, because it doesn't have a sane way of rate controlling the output, nor a sane way to determine how many buffers should be cached. If either of these problems were fixed, it would make sense to recycle the buffers to avoid random allocations, especially for small batches. These changes have something like a 10%-20% performance impact in the `dataflow-join` triangle computation workload.

## Support for non-serializable types

The communication layer is based on a type `Content<T>` which can be backed by typed or binary data. Consequently, it requires that the type it supports be serializable, because it needs to have logic for the case that the data is binary, even if this case is not used. It seems like the `Stream` type should be extendable to be parametric in the type of storage used for the data, so that we can express the fact that some types are not serializable and that this is ok. 

This would allow us to safely pass Rc<T> types around, as long as we use the `Pipeline` parallelization contract.

## Coarse- vs fine-grained timestamps

The progress tracking machinery involves some non-trivial overhead per timestamp. This means that using very fine-grained timestamps, for example the nanosecond at which a record is processed, swamps the progress tracking logic. By contrast, the logging infrastructure demotes nanoseconds to data, part of the logged payload, and approximates batches of events with the largest (should probably be the smallest) timestamp in the batch. This is less accurate from a progress tracking point of view, but more performant. It may be possible to generalize this so that users can write programs without thinking about granularity of timestamp, and the system automatically coarsens when possible (essentially boxcar-ing times).

<!--  ## Capability-based operators

At the moment there are somewhat delicate contracts with how operators should work: they should not hold on to timestamps not consumed in their inputs, and they should not produce messages for timestamps they have not held on to. This could be enforced by having the system hand out timestamps as capabilities, and attempting to make them unforgeable (in safe code, at least) to prevent accidental anti-patterns where timestamps just get stashed as keys it a `HashMap` and then used when the map is drained. As long as a timestamp is unaccounted for, literally there is an instance of it the systems hasn't had returned to it, the time is still considered open. -->

