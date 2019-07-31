// this module deals with parsing IRC message strings, and returning
// a Vec<&str> of parameters etc. to pass to the main protocol handlers
// earlier vesions of this code dealt too much with things specific to certain commands
// the parsers job should instead ensure messages conform to the standard structure
// as defined in Augmented BNF in the RFC 2812, but without ensuring
// that command-specific restrictions are adhered to (that will be done elsewhere in irc or
// irc::command or so)
// link: https://tools.ietf.org/html/rfc2812#section-2.3.1
// plus an optional source field (for server messages, indicating origin)
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr}
use crate::irc;
use crate::irc::rfc_defs as rfc;

// will want to change these types at some point
#[derive(Debug)]
pub enum ParseError {
    InvalidPrefix,
    NoCommand,
    InvalidCommand,
    InvalidNick,
    InvalidUser,
    InvalidHost
}

pub enum HostType {
    HostName(String),
    HostAddr(IpAddr)
}

pub enum MsgPrefix {
    Name(String), // generic for when we don't know if a name is a nickname or a hostname - special case
    NickHost(String, HostType),
    NickUserHost(String, String, HostType),
    Host(HostType)
}

pub struct ParseMsg {
    prefix: Option<MsgPrefix>,
    command: String,
    // NB: our parser first makes a Vec<&str>, where things will still point to stuff
    // in whatever the message slice sent to parse_message() was given a borrow of
    // params could also be a &[String], or an explicit array of 15 Strings,
    // but in the former case who owns the String array borrowed from?
    params: Option<Vec<String>>
}

// parsing IRC messages :)
// we want to use mostly &str operations for the parsing itself,
// but we don't want to have to care about the fate of message,
// so all the data structures we return will have new owned Strings
// also changed my mind about CRLF checking, whoever calls this function will be
// using CRLF delimiters to separate messages from the connection bytestream,
// therefore, it doesn't make sense to check for them or for them to even be
// included in the strings we get, same for empty lines - we shouldn't get any
//    Augmented BNF notation for general message strcture
//    message    =  [ ":" prefix SPACE ] command [ params ]
pub fn parse_message(message: &str) -> Result<ParsedMsg, ParseError> {
    // made get_prefix() code a bit nicer,
    // get_prefix checks if there is a prefix or not,
    // and returns both string slices as Option<&str>,
    // to help handle the odd case where there's a prefix but no content
    let (opt_prefix_string, opt_msg_body) = get_prefix(message);
    let msg_body: &str = if let Some(substr) = opt_msg_body {
        substr
    } else {
        // prefix string but no message body shouldn't really happen...
        return Err(ParseError::NoCommand)
    };

    // let's handle the case here where we have a prefix string,
    // now we want to parse it using the parse_prefix() function
    let opt_prefix: Option<MsgPrefix> = if let Some(prefix_string) = opt_prefix_string {
         match parse_prefix(prefix_string) {
             Ok(val) => opt_prefix = Some(val),
             Err(err_typ) => return Err(err_typ)
         }
    } else {
        None
    };
    
    // next we'll cut off the command part, that's fairly easy, we can index the first space and
    // then cut off a slice, we can also stop at a special case here and leave the rest of the
    // processing, if all we have is a command and no other parameters
    let command: String;
    let param_substring: &str;
    match msg_body.find(' ') {
        Some(index) => {
            command = &body[..index].to_string();
            if !rfc::is_valid_command(&command) {
                return Err(ParseError::InvalidCommand);
            }
            param_substring = &body[index+1..];
        }
        None => {
            command = body.to_string();
            if !rfc::is_valid_command(&command) {
                return Err(ParseError::InvalidCommand);
            } else {
                return Ok(ParseMsg {
                    prefix: opt_prefix,
                    command,
                    params: None
                });
            }
        }
    }

    // check for and split off the trailing argument
    let (middle, opt_trail) = split_colon_arg(&param_substring);
    let param_slices: Vec<&str>;
    match opt_trail {
        Some(trail_arg) => {
            // how many spaces would we have for 15 parameters? 14 spaces,
            // and if we have 15 parameters in *middle*, the last one has to
            // swallow up trailing - so we used .splitn() on the whole of param_substring
            if middle.split(' ').count() < rfc::MAX_MSG_PARAMS {
                // in this case, however, we only splitn on the middle part
                param_slices = middle.splitn(rfc::MAX_MSG_PARAMS - 1, ' ').collect();
                param_slices.push(&trail_arg);
            }
        }
        // this catches both the case of no trailing arg with a colon,
        // and the case where there is a " :" found, but there are already too many params
        _ => param_slices = param_substring.splitn(rfc::MAX_MSG_PARAMS, ' ').collect()
    }

    // now we've parsed them, but before giving them back to the caller, we want to copy everything
    // from the string slices into some new Vec<String>, which we can pass ownership of along
    let mut params: Vec<String> = Vec::new();
    for i in param_slices.iter() {
        params.push(i.to_string());
    }

    // return the stuff
    Ok(ParseMsg {
        opt_prefix,
        command,
        opt_params: Some(params)
    })
}

// this'll do a splitn(2, ' '), then return the command,
// plus optionally the rest of the message body, or None
// if the command has no parameters at all (which is a valid case)
// note one of the return values is a String! that's because
// the caller wants one, and we also give up ownership, not a borrow
fn get_command(msg_main: &str) -> (String, Option<&str>) {
    let substrings: Vec<&str> = msg_main.splitn(2, ' ').collect();
    match substrings.len() {
        1 => (substrings[0].to_string(), None),
        2 => (substrings[0].to_string(), Some(substrings[1])),
        _ => panic!("unhandled exception, should be impossible")
    }
}

fn get_prefix(message: &str) -> (Option<&str>, Option<&str>) {
    // if we have a prefix, we will first have a colon indicator
    // we know we will never have an empty line, but message.chars().nth(0) can give a
    // Some(whatever) or a None, so we have to explicitly check that, or use a string slice
    // this will panic if message is zero-length
    if &message[..1] == ":" {
        // check for a space
        let substrings: Vec<&str> = (&message[1..]).splitn(2, ' ').collect();
        match substrings.len() {
            1 => (Some(substrings[0]), None),
            2 => (Some(substrings[0]), Some(substrings[1])),
            _ => panic!("unhandled exception, should be impossible though")
        }
    } else {
        // no prefix found, so we just come back with only Some(message)
        // but a None for the prefix option
        (None, Some(message))
    }
}

// parse the prefix part of an IRC message
// with preceding colon and delimiting space stripped off
fn parse_prefix(msg: &str) -> Result<MsgPrefix, ParseError> {
    // start over with this...,
    // first, let's tokenize with '@'
    let first_split: Vec<&str> = msg.splitn(2, '@').collect();
    let name: &str = first_split[0];

    if first_split.len() == 2 {
        let host = first_split[1];
        // in this case we must have some sort of nick@host or possibly nick!user@host type
        // thing, so let's deal with that first...
        let second_split = first_split[0].splitn(2, '!').collect();
        if second_split.len() == 2 {
            let (nick, user) = (second_split[0], second_split[1]);
            if !rfc::valid_user(user) {
                Err(ParseError::InvalidUser)
            } else if !rfc::valid_nick(nick) {
                Err(ParseError::InvalidNick)
            } else {
                match parse_host(host) {
                    Ok(host_type) =>
                        Ok(MsgPrefix::NickUserHost(nick.to_string(), user.to_string(), host_type)),
                    Err(err_type) => Err(err_type)
                }
            }
        } else {
            let nick = name;
            if !rfc::valid_nick(nick) {
                Err(ParseError::InvalidNick)
            } else {
                match parse_host(host) {
                    Ok(host_type) =>
                        Ok(MsgPrefix::NickHost(nick.to_string(), host_type)),
                    Err(err_type) => Err(err_type)
                }
            }
        }
    } else {
        if !rfc::valid_nick(name) {
            // server case
            match parse_host(name) {
                Ok(host_type) => Ok(MsgPrefix::Host(name.to_string(), host_type)),  // we got a host :D
                Err(err_typ) => Err(err_typ) // something went wrong...
            }
        } else {
            // if we didn't get an @, and the nick is valid
            // we can't actually be totally sure if we have a 
            // nick or a host - tho we could rule out host with additional checks i suppose
            match parse_host(name) {
                Ok(_) => Ok(MsgPrefix::Name(name.to_string())),   // valid as host OR nick
                Err(_) => Ok(MsgPrefix::Nick(name.to_string()))     // only valid as nick
            }
        }
    }
}

// this host parsing code will assign whether we have a regular hostname (and if it's valid),
// or an ipv4/ipv6 address
fn parse_host(host_string: &str) -> Result<HostType, ParseError> {
    if rfc::valid_ipv4_addr(host_string) {
        Ok(HostType::IpAddr(Ipv4Addr::from_string(host_string)))
    } else if rfc::valid_ipv6_addr(host_string) {
        Ok(HostType::IpAddr(Ipv6Addr::from_string(host_string)))
    } else if rfc::valid_hostname(host_string) {
        Ok(HostType::HostName(host_string.to_string()))
    } else {
        Err(ParseError::InvalidHost)
    }
}
    
// this lil function snatches up a word and returns the rest of the string
// in an Option<String>, or just gives back the original String plus a None
fn split_colon_arg(msg: &str) -> (&str, Option<&str>) {
    if let Some(tail) = msg.find(" :") {
        (&msg[..tail], Some(&msg[tail+2..]))
    } else {
        (msg, None)
    }
}

