// this module contains a buffer type for reading and writing to sockets
// and will be involved in the transfer of control from the event system
// to the core irc protocol handlers

const MESSAGE_SIZE: usize = 512;
pub enum BufferError {
    OverFlow,
}

// might not always want this public
pub struct MessageBuffer {
    // the IRC protocol defines a maximum message size of 512 bytes,
    // including CR-LF. This being the case it doesn't make sense to
    // use buffers that resize as the client sends data, having a fixed
    // size will be generally faster due to simplified memory management
    buffer: [char; MESSAGE_SIZE],  // this needs to be char for String::extend() to work with a slice
    pub index: usize, // for incoming buffers we need some type of error handling
            // if we reach the end of the buffer and don't find CR-LF
}

impl MessageBuffer {
    // here we want to return the index of the next line, *after* CR-LF
    // so that the extract() fn spits out a string complete with carriage return
    // that will be stripped in parser::parse_message(&str) as that function expects
    // a correct-to-spec IRC message string
    fn get_eol(&self) -> Option<usize> {
        // anything past self.index is old (invalid!) data
        for i in 1..self.index {
            // byte literals are u8
            if self.buffer[i-1] == ('\r' as char) && self.buffer[i] == ('\n' as char) {
                return Some(i+1)
            }
        }
        None
    }

    // necessary to explicitly code for case where index is out of bounds? 
    // Rust should detect it and panic, I suppose
    fn shift_bytes_to_start(&mut self, start_index: usize) {
        // there's no need to copy everything to the very end of the buffer,
        // if it hasn't been completely filled
        for (i, j) in (start_index..self.index).enumerate() {
            self.buffer[i] = self.buffer[j];
        }
        self.index = self.index - start_index;  // there was a bug here! index should be shifted, not reset
    }

    // we only need this for client input buffers, so
    // might make more sense to implement in ClientIO?
    // then again its a task performed on the message buffer
    // and may prove to be more general
    // this probably should only be called when we know there's a CR-LF
    // to be found, but just incase we treat the no CR-LF case as
    // "return whatever string happens to currently be in there"
    pub fn extract(&mut self) -> Option<String> {
        if self.index == 0 {
            return None;
        }
        let mut out_string = String::new();
        if let Some(eol_index) = self.get_eol() {
            out_string.extend(&self.buffer[0..eol_index]);
            self.shift_bytes_to_start(eol_index);
        } else {
            out_string.extend(&self.buffer[..self.index]);
            self.index = 0;
        }
        Some(out_string)
    }

    // we also want code for appending to these buffers, more for server-> client writes
    // this can fail if the buffer doesn't have room for our message (probably indicates a connection problem)
    // for client buffers we're reading, this might be called by the incoming socket data event handler
    pub fn append(&mut self, message_string: String) -> Result<(), BufferError> {
        // how much space is left in the buffer?
        // does it make sense to try a partial write?
        if message_string.len() > (MESSAGE_SIZE - self.index) {
            return Err(BufferError::OverFlow);
        }
        for &byte in message_string.as_bytes() {
            self.buffer[self.index] = byte as char;
            self.index += 1;
        }
        return Ok(()); // returning Ok(current_index) as an output might be an option
    }

    pub fn new() -> MessageBuffer {
        MessageBuffer {
            buffer: [0 as char; MESSAGE_SIZE],
            index: 0,
        }
    }
}    
