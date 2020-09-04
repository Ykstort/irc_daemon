/* rusty-ircd - an IRC daemon written in Rust
*  Copyright (C) Joanna Janet Zaitseva-Doyle <jjadoyle@gmail.com>

*  This program is free software: you can redistribute it and/or modify
*  it under the terms of the GNU Lesser General Public License as
*  published by the Free Software Foundation, either version 3 of the
*  License, or (at your option) any later version.

*  This program is distributed in the hope that it will be useful,
*  but WITHOUT ANY WARRANTY; without even the implied warranty of
*  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
*  GNU Lesser General Public License for more details.

*  You should have received a copy of the GNU Lesser General Public License
*  along with this program.  If not, see <https://www.gnu.org/licenses/>.
*/
macro_rules! gef {
    ($e:expr) => (Err(GenError::from($e)));
}
pub mod chan;
pub mod error;
pub mod reply;
pub mod rfc_defs;
use crate::client;
use crate::client::{Client, ClientType, GenError, Host};
use crate::irc::chan::{ChanFlags, Channel};
use crate::irc::error::Error as ircError;
use crate::irc::reply::Reply as ircReply;
use crate::irc::rfc_defs as rfc;
use crate::parser::ParsedMsg;
extern crate log;
use log::{debug, info};
use std::clone::Clone;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

#[derive(Debug)]
pub enum NamedEntity {
    User(Weak<User>),
    Chan(Arc<Channel>),
}

impl Clone for NamedEntity {
    fn clone(&self) -> Self {
        match self {
            NamedEntity::User(ptr) => NamedEntity::User(Weak::clone(&ptr)),
            NamedEntity::Chan(ptr) => NamedEntity::Chan(Arc::clone(&ptr)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserFlags {
    registered: bool
}

#[derive(Debug)]
pub struct User {
    id: u64,
    nick: Mutex<String>,
    username: String,
    real_name: Mutex<String>,
    host: Host,
    channel_list: Mutex<HashMap<String, Weak<Channel>>>,
    flags: Mutex<UserFlags>,
    irc: Arc<Core>,
    client: Weak<Client>,
}

impl Clone for User {
    fn clone(&self) -> Self {
        User {
            id: self.id,
            nick: Mutex::new(self.nick.lock().unwrap().clone()),
            username: self.username.clone(),
            real_name: Mutex::new(self.real_name.lock().unwrap().clone()),
            host: self.host.clone(),
            channel_list: Mutex::new(self.channel_list.lock().unwrap().clone()),
            flags: Mutex::new(self.flags.lock().unwrap().clone()),
            irc: Arc::clone(&self.irc),
            client: Weak::clone(&self.client)
        }
    }
}

impl Drop for User {
    fn drop (&mut self) {
        debug!("drop called on user {}, clear channel list", self.get_nick());
        for (chan_name, chan_ptr) in self.channel_list.lock().unwrap().drain() {
            if let Some(chan) = Weak::upgrade(&chan_ptr) {
                if let Some(nick_key) = chan.get_user_key(&self.get_nick()) {
                    chan.rm_key(&nick_key);
                    if chan.is_empty() {
                        self.irc.remove_name(&chan_name);
                    }
                }
            }
        }
        self.irc.remove_name(&self.get_nick());
    }
}

impl User {
    pub fn new(
        id: u64,
        irc: &Arc<Core>,
        nick: String,
        username: String,
        real_name: String,
        host: client::Host,
        client: &Arc<Client>,
    ) -> Arc<Self> {
        Arc::new(User {
            id,
            irc: Arc::clone(&irc),
            nick: Mutex::new(nick),
            username,
            real_name: Mutex::new(real_name),
            host,
            channel_list: Mutex::new(HashMap::new()),
            client: Arc::downgrade(client),
            flags: Mutex::new(UserFlags { registered: true }), /*channel_list: Mutex::new(Vec::new())*/
        })
    }

    pub fn add_channel(&self, chanmask: &str, ptr: Weak<Channel>) {
        self.channel_list.lock().unwrap().insert(chanmask.to_string(), ptr);
    }

    // the functions that call this typically also deal with delinking
    // our user from the chan's user list, and remove from server channel
    // hashmap if the channel is empty, so no need to call self._rm_chan_inside_lock()
    pub fn rm_channel(&self, chanmask: &str) {
        self.channel_list.lock().unwrap().remove(chanmask);
    }

    // separate function allows this one to be called from clear_chans_and_exit(),
    // which has an outer channel_list mutex lock
    fn _rm_chan_inside_lock(&self, chanmask: &str, ptr: &Channel) {
        if let Some(key) = ptr.get_user_key(&self.get_nick()) {
            ptr.rm_key(&key);
            if ptr.is_empty() {
                if let Ok(NamedEntity::Chan(_ptr)) = self.irc.remove_name(chanmask) {
                    debug!("removed chan {} on part/exit of user {}", chanmask, self.get_nick());
                } else {
                    debug!("attempt to remove chan {} from main server hashmap failed", chanmask);
                }
            }
        } else {
            debug!("chan {} was in user {}'s HashMap, but the relationship was not mutual",
                    chanmask, self.get_nick());
        }
    }

    /* Drop code should take care of this, but I'll leave in
     * a couple of canaries that will report on things not being
     * properly freed */
    pub fn cleanup(irc: &Core, nick: &str) {
        debug!("nick {} was tied to a dangling ref - removing", nick);
        let ret = irc.remove_name(nick);
        if ret.is_ok() { debug!("removed {} from IRC namespace hash", nick); }
        irc.search_user_chans_purge(nick);
    }

    pub fn clear_chans_and_exit(&self) -> Vec<Arc<Channel>> {
        let mut witnesses = Vec::new();

        /* by using drain() here we properly clear the map */
        for (chan_name, chan_ptr) in self.channel_list.lock().unwrap().drain() {
            /* gotta be careful unwrapping on these, has lead to panic
             * in another situation, tho that was a bug */
            if let Some(chan) = Weak::upgrade(&chan_ptr) {
                self._rm_chan_inside_lock(&chan_name, &chan);
                if !chan.is_empty() {
                    witnesses.push(chan);
                }
            } else {
                debug!("cleanup of user {}, can't remove channel {}, possibly already freed",
                        self.get_nick(), chan_name);
            }
        }

        witnesses
    }

    pub fn clear_channel_list(&self) {
        self.channel_list.lock().unwrap().clear()
    }

    /* attempt to find and upgrade a pointer to the user's client,
     * if that fails, so some cleanup and return an error indicating
     * dead client or similar */
    pub fn fetch_client(self: &Arc<Self>) -> Result<Arc<Client>, GenError> {
        if let Some(client) = Weak::upgrade(&self.client) {
            Ok(client)
        } else {
            let wits = self.clear_chans_and_exit();
            debug!("got a dead client @ user {}", self.get_nick());
            /* can't iterate here as chan.notify_quit() will call
             * user.send_line() and make this fn recursive */
            Err(GenError::DeadClient(Arc::clone(&self), wits))
        }
    }

    /* nick changes need to be done carefully and atomically, or they'll
     * lead to race conditions and mess with book-keeping (unless I stop
     * relying on purely text based keys for some User/Channel management) */
    pub fn change_nick(self: &Arc<Self>, name: &str) -> Result<ircReply, GenError> {
        self.irc.try_nick_change(self, name)
    }

    pub fn get_id(&self) -> u64 {
        self.id
    }

    pub fn get_channel_list(&self) -> Vec<Weak<Channel>> {
        let mut values = Vec::new();
        for val in self.channel_list.lock().unwrap().values() {
            values.push(Weak::clone(&val));
        }
        values
    }

    pub fn get_nick(&self) -> String {
        self.nick.lock().unwrap().clone()
    }

    pub fn get_username(&self) -> String {
        self.username.clone()
    }

    pub fn get_host(&self) -> Host {
        match &self.host {
            Host::Hostname(name) => Host::Hostname(name.clone()),
            Host::HostAddr(ip_addr) => Host::HostAddr(*ip_addr),
        }
    }

    pub fn get_host_string(&self) -> String {
        match &self.host {
            Host::Hostname(name) => name.to_string(),
            Host::HostAddr(ip_addr) => ip_addr.to_string(),
        }
    }

    pub fn get_realname(&self) -> String {
        self.real_name.lock().unwrap().clone()
    }

    pub fn get_prefix(&self) -> String {
        format!(
            "{}!{}@{}",
            self.get_nick(),
            self.username,
            self.get_host_string()
        )
    }

    pub async fn send_msg(
        self: &Arc<Self>,
        src: &User,
        command_str: &str,
        target: &str,
        msg: &str
    ) -> Result<ircReply, GenError> {
        let prefix = src.get_prefix();
        let line = format!(":{} {} {} :{}", &prefix, command_str, target, msg);
        /* instead of unwrap(), fetch_client() tries to upgrade the pointer,
         * if that fails it does some cleaning up and returns a GenError::Io(unexpected Eof)
         */
        let my_client = self.fetch_client()?;
        /* passing to an async fn and awaiting on it is gonna
         * cause lifetime problems with a &str... */
        my_client.send_line(&line).await?;
        Ok(ircReply::None)
    }

    pub async fn send_err(self: &Arc<Self>, err: ircError) -> Result<ircReply, GenError> {
        let line = format!(":{} {}", self.irc.get_host(), err);
        let my_client = self.fetch_client()?;
        /* passing to an async fn and awaiting on it is gonna
         * cause lifetime problems with a &str... */
        my_client.send_line(&line).await?;
        Ok(ircReply::None)
    }

    pub async fn send_rpl(self: &Arc<Self>, reply: ircReply) -> Result<ircReply, GenError> {
        /* passing to an async fn and awaiting on it is gonna
         * cause lifetime problems with a &str... */
        let host = self.irc.get_host();
        let line = format!(":{} {}", host, reply);
        if line.len() > rfc::MAX_MSG_SIZE - 2 {
            match reply {
                /* not all can be recursed */
                ircReply::NameReply(chan, mut nick_vec) => {
                    /* "353 {} :{}<CR><LF>" */
                    let overhead = rfc::MAX_MSG_PARAMS - (10 + chan.len() + host.len());
                    let mut vec_len = nick_vec.len();
                    let mut i = 0;
                    let mut sum = 0;

                    /* count how many strings we can fit */
                    while i < vec_len {
                        if sum + nick_vec[i].len() >= overhead {
                            let temp = nick_vec.split_off(i);
                            let line = format!(":{} {}", host, ircReply::NameReply(chan.clone(), nick_vec));
                            let my_client = self.fetch_client()?;
                            my_client.send_line(&line).await?;
                            nick_vec = temp;
                            i = 0;
                            sum = 0;
                            vec_len = nick_vec.len();
                        }
                    }

                    Ok(ircReply::None)
                }
                _ => Ok(ircReply::None),
            }
        } else {
            let my_client = self.fetch_client()?;
            my_client.send_line(&line).await?;
            Ok(ircReply::None)
        }
    }

    pub async fn send_line(self: &Arc<Self>, line: &str) -> Result<ircReply, GenError> {
        let my_client = self.fetch_client()?;
        /* passing to an async fn and awaiting on it is gonna
         * cause lifetime problems with a &str... */
        my_client.send_line(line).await?;
        Ok(ircReply::None)
    }

    pub fn upgrade(weak_ptr: &Weak<Self>, nick: &str) -> Result<Arc<Self>, GenError> {
        if let Some(good_ptr) = Weak::upgrade(&weak_ptr) {
            Ok(good_ptr)
        } else {
            Err(GenError::DeadUser(nick.to_string()))
        }
    }
}

#[derive(Debug)]
pub struct ProtoUser {
    nick: Option<String>,
    username: Option<String>,
    real_name: Option<String>,
}

#[derive(Debug)]
pub struct Core {
    namespace: Mutex<HashMap<String, NamedEntity>>,
    clients: Mutex<HashMap<u64, Weak<Client>>>,
    id_counter: Mutex<u64>, //servers: Mutex<HashMap<u64, Arc<Server>>>,
    hostname: String
}

impl Core {
    // init hash tables
    pub fn new(hostname: String) -> Arc<Self> {
        let clients = Mutex::new(HashMap::new());
        //let servers  = Mutex::new(HashMap::new());
        let namespace = Mutex::new(HashMap::new());
        let id_counter = Mutex::new(0);
        Arc::new(Core {
            clients,
            namespace, // combined nick and channel HashMap
            id_counter, //servers
            hostname
        })
    }

    pub fn assign_id(&self) -> u64 {
        let mut lock_ptr = self.id_counter.lock().unwrap();
        *lock_ptr += 1;
        *lock_ptr
    }

    pub fn insert_client(&self, id: u64, client: Weak<Client>) {
        self.clients.lock().unwrap().insert(id, client);
    }

    pub fn insert_name(&self, name: &str, item: NamedEntity) -> Result<(), ircError> {
        let mut hashmap = self.namespace.lock().unwrap();
        if !hashmap.contains_key(name) {
            hashmap.insert(name.to_string(), item);
            debug!("added key {} hashmap, size = {}", name, hashmap.len());
            Ok(())
        } else {
            Err(ircError::NicknameInUse(name.to_string()))
        }
    }

    pub fn remove_name(&self, name: &str) -> Result<NamedEntity, ircError> {
        let mut hashmap = self.namespace.lock().unwrap();
        let ret = hashmap
            .remove(name)
            .ok_or_else(|| ircError::NoSuchNick(name.to_string()));
        if ret.is_ok() {
            debug!("removed key {} from hashmap, size = {}", name, hashmap.len());
        }
        ret
    }

    pub fn get_host(&self) -> String {
        self.hostname.clone()
    }

    pub fn get_client(&self, id: &u64) -> Option<Weak<Client>> {
        self.clients
            .lock()
            .unwrap()
            .get(id)
            .map(|cli| Weak::clone(cli))
    }

    pub fn remove_client(&self, id: &u64) -> Option<Weak<Client>> {
        self.clients.lock().unwrap().remove(id)
    }

    pub fn get_name(&self, name: &str) -> Option<NamedEntity> {
        self.namespace.lock().unwrap().get(name).cloned()
    }

    pub fn get_nick(&self, nick: &str) -> Option<Weak<User>> {
        if let Some(NamedEntity::User(u_ptr)) = self.get_name(nick) {
            Some(u_ptr)
        } else {
            None
        }
    }

    pub fn get_chan(&self, chanmask: &str) -> Option<Arc<Channel>> {
        if let Some(NamedEntity::Chan(chan)) = self.get_name(chanmask) {
            Some(chan)
        } else {
            None
        }
    }

    pub async fn part_chan(
        &self,
        chanmask: &str,
        user: &Arc<User>,
        part_msg: &str,
    ) -> Result<ircReply, GenError> {
        if let Some(chan) = self.get_chan(chanmask) {
            if !chan.is_joined(&user.get_nick()) {
                gef!(ircError::NotOnChannel(chanmask.to_string()))
            } else {
                user.rm_channel(chanmask);
                if let Some(key) = chan.get_user_key(&user.get_nick()) {
                    chan.rm_key(&key);
                }
                if chan.is_empty() {
                    self.remove_name(chanmask)?; Ok(ircReply::None)
                } else {
                    chan.notify_part(user, chanmask, part_msg).await; Ok(ircReply::None)
                }
            }
        } else {
            gef!(ircError::NoSuchChannel(chanmask.to_string()))
        }
    }

    pub async fn join_chan(self: &Arc<Core>, chanmask: &str, user: &Arc<User>) -> Result<ircReply, GenError> {
        if !rfc::valid_channel(chanmask) {
            return gef!(ircError::NoSuchChannel(chanmask.to_string()));
        }

        let channel = self.get_chan(chanmask);
        let chan = if let Some(chan) = channel {
            /* need to check if user is already in chan */
            if chan.is_joined(&user.get_nick()) {
                return Ok(ircReply::None);
            }
            chan.add_user(user, ChanFlags::None);
            chan.notify_join(user, chanmask).await;
            chan
        } else {
            let chan = Arc::new(Channel::new(&self, chanmask));
            self.insert_name(chanmask, NamedEntity::Chan(Arc::clone(&chan)))?; // what happens if this error does occur?
            chan.add_user(user, ChanFlags::Op);
            chan
        };

        user.add_channel(chanmask, Arc::downgrade(&chan));

        user.send_rpl(ircReply::Topic(chanmask.to_string(), chan.get_topic()))
            .await?;

        user.send_rpl(ircReply::NameReply(
            chanmask.to_string(),
            chan.gen_sorted_nick_list(),
        ))
        .await?;

        Ok(ircReply::EndofNames(chanmask.to_string()))
    }

    /* don't want anyone to take our nick while we're in the middle of faffing around... */
    pub fn try_nick_change(&self, user: &User, new_nick: &str) -> Result<ircReply, GenError> {
        let mut big_fat_mutex_lock = self.namespace.lock().unwrap();
        let mut chanlist_mutex_lock = user.channel_list.lock().unwrap();
        let nick = new_nick.to_string();
        let old_nick = user.get_nick();
        if big_fat_mutex_lock.contains_key(&nick) {
            gef!(ircError::NicknameInUse(nick))
        } else {
            if let Some(val) = big_fat_mutex_lock.remove(&old_nick) {
                /* move to new key */
                big_fat_mutex_lock.insert(nick.clone(), val);

                /* update User struct */
                *user.nick.lock().unwrap() = nick;

                /* update channels list */
                for (chan_name, chan_wptr) in chanlist_mutex_lock.clone().iter() {
                    if let Some(chan) = Weak::upgrade(&chan_wptr) {
                        chan.update_nick(&old_nick, &new_nick);
                    } else {
                        debug!("can't upgrade pointer to {}, deleting key", chan_name);
                        chanlist_mutex_lock.remove(chan_name);
                    }
                }
            }
            Ok(ircReply::None)
        }
    }

    pub fn register(
        &self,
        client: &Arc<Client>,
        nick: String,
        username: String,
        real_name: String,
    ) -> Result<Arc<User>, ircError> {
        let host_str = client.get_host_string();
        let host = client.get_host();
        let id = client.get_id();
        let irc = client.get_irc();
        debug!(
            "register user {}!{}@{}, Real name: {} -- client id {}",
            &nick, &username, &host_str, &real_name, id
        );
        let user = User::new(
            id,
            irc,
            nick.to_string(),
            username,
            real_name,
            host.clone(),
            client,
        );
        self.insert_name(&nick, NamedEntity::User(Arc::downgrade(&user)))?;
        Ok(user)
    }

    fn _search_user_chans(&self, nick: &str, purge: bool) -> Vec<String> {
        let mut channels = Vec::new();
        let mut chan_strings = Vec::new();
        for value in self.namespace.lock().unwrap().values() {
            if let NamedEntity::Chan(chan_ptr) = value {
                channels.push(Arc::clone(&chan_ptr));
            }
        }

        for channel in channels.iter() {
            if channel.is_empty() && self.remove_name(&channel.get_name()).is_ok() {
                debug!("remove channel {} from IRC HashMap", &channel.get_name());
            }
            if let Some(key) = channel.get_user_key(nick) {
                chan_strings.push(channel.get_name());
                if purge {
                    channel.rm_key(&key);
                }
            }
        }

        chan_strings
    }

    pub fn search_user_chans(&self, nick: &str) -> Vec<String> {
        self._search_user_chans(nick, false)
    }

    pub fn search_user_chans_purge(&self, nick: &str) -> Vec<String> {
        self._search_user_chans(nick, true)
    }
}

#[derive(Debug)]
pub enum MsgType {
    PrivMsg,
    Notice,
}

pub async fn command(irc: &Arc<Core>, client: &Arc<Client>, params: ParsedMsg) -> Result<ircReply, GenError> {
    let registered = client.is_registered();
    let cmd = params.command.to_ascii_uppercase();

    match &cmd[..] {
        "NICK" => nick(irc, client, params).await,
        "USER" => user(irc, client, params).await,
        "PRIVMSG" if registered => msg(irc, &client.get_user(), params, false).await,
        "NOTICE" if registered => msg(irc, &client.get_user(), params, true).await,
        "JOIN" if registered => join(irc, &client.get_user(), params).await,
        "PART" if registered => part(irc, &client.get_user(), params).await,
        "TOPIC" if registered => topic(irc, &client.get_user(), params).await,
        "PART" | "JOIN" | "PRIVMSG" | "NOTICE" | "TOPIC" if !registered => gef!(ircError::NotRegistered),
        _ => gef!(ircError::UnknownCommand(params.command.to_string())),
    }
}

pub async fn topic(irc: &Core, user: &User, mut params: ParsedMsg) -> Result<ircReply, GenError> {
    if params.opt_params.is_empty() {
        return gef!(ircError::NeedMoreParams("TOPIC".to_string()));
    }

    let chanmask = params.opt_params.remove(0);
    /* just get the topic */
    if let Some(chan) = irc.get_chan(&chanmask) {
        if chan.is_joined(&user.get_nick()) {
            if params.opt_params.is_empty() {
                Ok(ircReply::Topic(chanmask, chan.get_topic()))
            } else if chan.is_op(user) {
                chan.set_topic(&params.opt_params.remove(0));
                Ok(ircReply::None)
            } else {
                gef!(ircError::ChanOPrivsNeeded(chanmask))
            }
        } else {
            gef!(ircError::NotOnChannel(chanmask))
        }
    } else {
        gef!(ircError::NoSuchChannel(chanmask))
    }
}

pub async fn join(irc: &Arc<Core>, user: &Arc<User>, mut params: ParsedMsg) -> Result<ircReply, GenError> {
    if params.opt_params.is_empty() {
        return gef!(ircError::NeedMoreParams("JOIN".to_string()));
    }

    /* JOIN can take a second argument. The format is:
     * JOIN comma,sep.,chan,list comma,sep.,key,list
     * but I'll leave key implementation til later */
    let targets = params.opt_params.remove(0);
    for target in targets.split(',') {
        match irc.join_chan(&target, user).await {
            Err(GenError::IRC(err)) => user.send_err(err).await?,
            Err(other_err) => return Err(other_err),
            Ok(reply) => user.send_rpl(reply).await?,
        };
    }
    Ok(ircReply::None)
}

pub async fn part(irc: &Arc<Core>, user: &Arc<User>, mut params: ParsedMsg) -> Result<ircReply, GenError> {
    if params.opt_params.is_empty() {
        return gef!(ircError::NeedMoreParams("PART".to_string()));
    }

    let targets = params.opt_params.remove(0);
    let part_msg = if params.opt_params.is_empty() {
        String::from("")
    } else {
        params.opt_params.remove(0)
    };
    for target in targets.split(',') {
        match irc.part_chan(&target, user, &part_msg).await {
            Err(GenError::IRC(err)) => { user.send_err(err).await?; },
            Err(err) => { debug!("{} PART {}: {}", user.get_nick(), target, err); },
            _ => (),
        };
    }
    Ok(ircReply::None)
}

pub async fn msg(
    irc: &Core,
    send_u: &Arc<User>,
    mut params: ParsedMsg,
    notice: bool,
) -> Result<ircReply, GenError> {
    if params.opt_params.is_empty() {
        return if notice { Ok(ircReply::None) } else { gef!(ircError::NoRecipient("PRIVMSG".to_string())) };
    }
    /* this appears to be what's crashing, despite the check for params.opt_params.is_empty() beforehand
     * ah, I'd forgotten to remove one of the notice bools from the above if statements,
     * if params.opt_params.is_empty() && notice won't work */
    let targets = params.opt_params.remove(0); 
    let cmd = if notice { "NOTICE" } else { "PRIVMSG" };

    // if there were no more args, message should be an empty String
    if params.opt_params.is_empty() {
        return if notice { Ok(ircReply::None) } else { gef!(ircError::NoTextToSend) };
    }
    // if there are more than two arguments,
    // concatenate the remainder to one string
    let message = params.opt_params.join(" ");
    debug!("{} from user {} to {}, content: {}", cmd, send_u.get_nick(), targets, message);

    // loop over targets
    for target in targets.split(',') {
        let result = match irc.get_name(target) {
            Some(NamedEntity::User(user_weak)) => {
                match User::upgrade(&user_weak, target) {
                    Ok(recv_u) => recv_u.send_msg(&send_u, &cmd, &target, &message).await,
                    Err(GenError::DeadUser(nick)) => {
                        User::cleanup(irc, &nick);
                        Err(GenError::DeadUser(nick))
                    },
                    Err(e) => Err(e),
                }
            },
            Some(NamedEntity::Chan(chan))
                => chan.send_msg(&send_u, &cmd, &target, &message).await,
            None => gef!(ircError::NoSuchNick(target.to_string()))
        };
        match result {
            Err(GenError::IRC(err)) if !notice => {
                send_u.send_err(err).await?;
            },
            Err(any_err) => {
                debug!("error sending message to {}: {}", target, any_err);
            },
            _ => (),
        }
    }
    Ok(ircReply::None)
}

pub async fn user(irc: &Core, client: &Arc<Client>, params: ParsedMsg) -> Result<ircReply, GenError> {
    // a USER command should have exactly four parameters
    // <username> <hostname> <servername> <realname>,
    // though we ignore the middle two unless a server is
    // forwarding the message
    let args = params.opt_params;
    if args.len() != 4 {
        return gef!(ircError::NeedMoreParams("USER".to_string()));
    }
    let username = args[0].clone();
    let real_name = args[3].clone();

    let result = match client.get_client_type() {
        ClientType::Dead => None,
        ClientType::Unregistered => {
            // initiate handshake
            Some(ClientType::ProtoUser(Arc::new(Mutex::new(ProtoUser {
                nick: None,
                username: Some(username),
                real_name: Some(real_name),
            }))))
        }
        ClientType::User(_user_ref) => {
            // already registered! can't change username
            return gef!(ircError::AlreadyRegistred);
        }
        ClientType::ProtoUser(proto_user_ref) => {
            // got nick already? if so, complete registration
            let proto_user = proto_user_ref.lock().unwrap();
            if let Some(nick) = &proto_user.nick {
                // had nick already, complete registration
                Some(ClientType::User(
                    irc.register(client, nick.clone(), username, real_name)?, // propagate the error if it goes wrong
                )) // (nick taken, most likely corner-case)
                   // there probably is some message we're meant to
                   // return to the client to confirm successful
                   // registration...
            } else {
                // don't see an error in the irc file,
                // except the one if you're already reg'd
                // NOTICE_BLOCKY
                proto_user_ref.lock().unwrap().username = Some(username);
                proto_user_ref.lock().unwrap().real_name = Some(real_name);
                None
            }
        } //ClientType::Server(_server_ref) => (None, None, false)
    };

    if let Some(new_client_type) = result {
        client.set_client_type(new_client_type);
    }
    Ok(ircReply::None)
}

pub async fn nick(irc: &Core, client: &Arc<Client>, params: ParsedMsg) -> Result<ircReply, GenError> {
    let nick;
    if let Some(n) = params.opt_params.iter().next() {
        nick = n.to_string();
    } else {
        return gef!(ircError::NeedMoreParams("NICK".to_string()));
    }

    // is the nick a valid nick string?
    if !rfc::valid_nick(&nick) {
        return gef!(ircError::ErroneusNickname(nick));
    }

    // is this nick already taken?
    if let Some(_hit) = irc.get_name(&nick) {
        return gef!(ircError::NicknameInUse(nick));
    }

    // we can return a tuple and send messages after the match
    // to avoid borrowing mutably inside the immutable borrow
    // (Some(&str), Some(ClientType), bool died)
    let result = match client.get_client_type() {
        ClientType::Dead => None,
        ClientType::Unregistered => {
            // in this case we need to create a "proto user"
            Some(ClientType::ProtoUser(Arc::new(Mutex::new(ProtoUser {
                nick: Some(nick),
                username: None,
                real_name: None,
            }))))
        }
        ClientType::User(user_ref) => {
            // just a nick change
            user_ref.change_nick(&nick)?;
            None
        }
        ClientType::ProtoUser(proto_user_ref) => {
            // in this case we already got USER
            let mut proto_user = proto_user_ref.lock().unwrap();
            // need to account for the case where NICK is sent
            // twice without any user command
            if proto_user.nick.is_some() {
                proto_user.nick = Some(nick);
                None
            } else {
                // full registration! wooo
                let username = proto_user.username.as_ref();
                let real_name = proto_user.real_name.as_ref();
                Some(ClientType::User(
                    irc.register(
                        client,
                        nick,
                        username.unwrap().to_string(),
                        real_name.unwrap().to_string(),
                    )?, // error propagation if registration fails
                ))
            }
        }
    };

    if let Some(new_client_type) = result {
        client.set_client_type(new_client_type);
    }
    Ok(ircReply::None)
}
