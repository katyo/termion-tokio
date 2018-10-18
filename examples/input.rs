extern crate futures;
extern crate termion;
extern crate termion_tokio;
extern crate tokio;

use std::io::stdout;
use std::mem::drop;
use std::process::exit;
use std::time::Duration;

use futures::{future::Either, lazy, Future, Stream};
use tokio::{io::stdin, run, timer::Interval};

use termion::{event::Key, raw::IntoRawMode};
use termion_tokio::input::TermReadAsync;

fn main() {
    run(lazy(|| {
        let raw_term = stdout()
            .into_raw_mode()
            .map_err(|er| eprintln!("Error: {:?}", er))
            .unwrap();

        let input = stdin().keys_stream().map(Either::B).map_err(Either::B);
        let ticks = Interval::new_interval(Duration::from_secs(3))
            .map(Either::A)
            .map_err(Either::A);

        let events = ticks.select(input);

        events
            .fold(raw_term, |raw_term, it| {
                println!("Event: {:?}\r", it);
                match it {
                    Either::B(Key::Esc)
                    | Either::B(Key::Char('q'))
                    | Either::B(Key::Ctrl('c'))
                    | Either::B(Key::Ctrl('b')) => {
                        drop(raw_term);
                        exit(0);
                    }
                    _ => (),
                }
                Ok(raw_term)
            }).map(|_| ())
            .map_err(|er| {
                eprintln!("Error: {:?}", er);
            })
    }));
}
