use std::io::{self, Write};
use std::iter::empty;
use std::mem::replace;

use bytes::{Buf, BytesMut, IntoBuf};
use futures::{Async, Future, Poll, Stream};
use tokio::{
    codec::{Decoder, FramedRead},
    io::AsyncRead,
};

use termion::event::{self, Event, Key};
use termion::raw::IntoRawMode;

/// A stream of input keys.
pub struct KeysStream<R> {
    inner: EventsStream<R>,
}

impl<R: AsyncRead> Stream for KeysStream<R> {
    type Item = Key;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        use self::Async::*;
        self.inner.poll().map(|async| match async {
            Ready(Some(Event::Key(k))) => Ready(Some(k)),
            Ready(Some(_)) => NotReady,
            Ready(None) => Ready(None),
            NotReady => NotReady,
        })
    }
}

/// An iterator over input events.
pub struct EventsStream<R> {
    inner: EventsAndRawStream<R>,
}

impl<R: AsyncRead> Stream for EventsStream<R> {
    type Item = Event;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.inner
            .poll()
            .map(|async| async.map(|option| option.map(|(event, _raw)| event)))
    }
}

/// An iterator over input events and the bytes that define them
type EventsAndRawStream<R> = FramedRead<R, EventsAndRawDecoder>;

pub struct EventsAndRawDecoder;

impl Decoder for EventsAndRawDecoder {
    type Item = (Event, Vec<u8>);
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match src.len() {
            0 => Ok(None),
            1 => match src[0] {
                b'\x1B' => {
                    src.advance(1);
                    Ok(Some((Event::Key(Key::Esc), vec![b'\x1B'])))
                }
                c => {
                    if let Ok(res) = parse_event(c, &mut empty()) {
                        src.advance(1);
                        Ok(Some(res))
                    } else {
                        Ok(None)
                    }
                }
            },
            _ => {
                let (off, res) = if let Some((c, cs)) = src.split_first() {
                    let cur = cs.into_buf();
                    let mut it = cur.iter().map(Ok);
                    if let Ok(res) = parse_event(*c, &mut it) {
                        (1 + cs.len() - it.len(), Ok(Some(res)))
                    } else {
                        (0, Ok(None))
                    }
                } else {
                    (0, Ok(None))
                };

                src.advance(off);
                res
            }
        }
    }
}

fn parse_event<I>(item: u8, iter: &mut I) -> Result<(Event, Vec<u8>), io::Error>
where
    I: Iterator<Item = Result<u8, io::Error>>,
{
    let mut buf = vec![item];
    let result = {
        let mut iter = iter.inspect(|byte| {
            if let &Ok(byte) = byte {
                buf.push(byte);
            }
        });
        event::parse_event(item, &mut iter)
    };
    result
        .or(Ok(Event::Unsupported(buf.clone())))
        .map(|e| (e, buf))
}

enum ReadLineState<R> {
    Error(io::Error),
    Read(Vec<u8>, R),
    Done,
}

pub struct ReadLineFuture<R>(ReadLineState<R>);

impl<R> From<io::Error> for ReadLineFuture<R> {
    fn from(error: io::Error) -> Self {
        ReadLineFuture(ReadLineState::Error(error))
    }
}

impl<R> ReadLineFuture<R> {
    fn new(source: R) -> Self {
        ReadLineFuture(ReadLineState::Read(Vec::with_capacity(30), source))
    }
}

impl<R: AsyncRead> Future for ReadLineFuture<R> {
    type Item = (Option<String>, R);
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        use self::ReadLineState::*;
        match replace(&mut self.0, Done) {
            Error(error) => Err(error),
            Read(mut buf, mut input) => {
                use self::Async::*;

                let mut byte = [0x8, 1];

                match input.poll_read(&mut byte) {
                    Ok(Ready(1)) => match byte[0] {
                        0 | 3 | 4 => return Ok(Ready((None, input))),
                        0x7f => {
                            buf.pop();
                        }
                        b'\n' | b'\r' => {
                            let string = try!(
                                String::from_utf8(buf)
                                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                            );
                            return Ok(Ready((Some(string), input)));
                        }
                        c => {
                            buf.push(c);
                        }
                    },
                    Ok(Ready(_)) => (),
                    Ok(NotReady) => (),
                    Err(e) => return Err(e),
                }

                self.0 = Read(buf, input);
                Ok(NotReady)
            }
            Done => unreachable!(),
        }
    }
}

/// Extension to `Read` trait.
pub trait TermReadAsync: Sized {
    /// An iterator over input events.
    fn events_stream(self) -> EventsStream<Self>
    where
        Self: Sized;

    /// An iterator over key inputs.
    fn keys_stream(self) -> KeysStream<Self>
    where
        Self: Sized;

    /// Read a line.
    ///
    /// EOT and ETX will abort the prompt, returning `None`. Newline or carriage return will
    /// complete the input.
    fn read_line_future(self) -> ReadLineFuture<Self>;

    /// Read a password.
    ///
    /// EOT and ETX will abort the prompt, returning `None`. Newline or carriage return will
    /// complete the input.
    fn read_passwd_future<W: Write>(self, writer: &mut W) -> ReadLineFuture<Self> {
        if let Err(error) = writer.into_raw_mode() {
            error.into()
        } else {
            self.read_line_future()
        }
    }
}

impl<R: AsyncRead + TermReadAsyncEventsAndRaw> TermReadAsync for R {
    fn events_stream(self) -> EventsStream<Self> {
        EventsStream {
            inner: self.events_and_raw_stream(),
        }
    }
    fn keys_stream(self) -> KeysStream<Self> {
        KeysStream {
            inner: self.events_stream(),
        }
    }

    fn read_line_future(self) -> ReadLineFuture<Self> {
        ReadLineFuture::new(self)
    }
}

/// Extension to `TermReadAsync` trait. A separate trait in order to maintain backwards compatibility.
pub trait TermReadAsyncEventsAndRaw {
    /// An iterator over input events and the bytes that define them.
    fn events_and_raw_stream(self) -> EventsAndRawStream<Self>
    where
        Self: Sized;
}

impl<R: AsyncRead> TermReadAsyncEventsAndRaw for R {
    fn events_and_raw_stream(self) -> EventsAndRawStream<Self> {
        FramedRead::new(self, EventsAndRawDecoder)
    }
}
