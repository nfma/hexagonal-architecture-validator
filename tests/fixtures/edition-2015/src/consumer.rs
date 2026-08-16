use std::io;
use write::Flush;

pub fn flush() -> Flush {
    Flush
}

pub fn sink() -> io::Sink {
    io::sink()
}
