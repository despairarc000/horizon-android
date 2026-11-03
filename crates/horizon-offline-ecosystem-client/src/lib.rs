//! Offline, headless player protocol over an in-memory Pipe.
//! The caller runs the authoritative Minecraft integrated server concurrently.
//! Nothing here is a network service, Minecraft AI, or a replacement for server rules.

use std::collections::HashSet;
use horizon_protocol::{
    PROTOCOL_VERSION,
    buf::{Reader,Writer},
    connection::{Connection,Pipe},
    crypto::offline_uuid,
    ids,
    packets::{Empty,configuration as cfg,handshake,login,play},
};

#[derive(Clone,Copy,Debug,Default,PartialEq)]
pub struct State {
    pub position: Option<[f64;3]>,
    pub yaw: f32,
    pub pitch: f32,
    pub health: Option<f32>,
    pub hunger: Option<i32>,
    pub ready: bool,
    pub received_chunks: u64,
    pub teleports: u64,
}

pub struct LocalPlayer {
    conn: Connection,
    name: String,
    pub state: State,
    chunks: HashSet<(i32,i32)>,
    announced_loaded: bool,
}

impl LocalPlayer {
    /// Blocking login and configuration. Call on a worker thread.
    pub fn join(pipe:Pipe,name:&str,view_distance:i32)->Result<Self,String>{
        if name.is_empty() || name.len()>16 ||
           !name.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'){
            return Err("bot name must be 1-16 ASCII letters, numbers or underscores".into());
        }
        if !(2..=10).contains(&view_distance){
            return Err("view distance must be between 2 and 10".into());
        }
        let err=|e:horizon_protocol::Error|e.to_string();
        let mut conn=Connection::local(pipe);
        conn.send(&handshake::Serverbound::Intention(handshake::Intention{
            protocol:PROTOCOL_VERSION,host:"local".into(),port:0,intent:handshake::LOGIN,
        }).encode()).map_err(err)?;
        conn.send(&login::Serverbound::Hello(login::ServerHello{
            name:name.into(),uuid:offline_uuid(name),
        }).encode()).map_err(err)?;
        loop{
            let (id,body)=conn.recv().map_err(err)?;
            match login::Clientbound::decode(id,&body).map_err(err)?{
                login::Clientbound::Compression(p)=>conn.set_compression(p.threshold),
                login::Clientbound::LoginFinished(p)=>{
                    if p.profile.name!=name || p.profile.uuid!=offline_uuid(name){
                        return Err("server supplied wrong local profile".into());
                    }
                    conn.send(&login::Serverbound::LoginAcknowledged(Empty).encode()).map_err(err)?;
                    break;
                },
                login::Clientbound::Hello(_)=>return Err("online login cannot be used by local offline bots".into()),
                login::Clientbound::Disconnect(p)=>return Err(format!("login rejected: {}",p.reason_json)),
                login::Clientbound::CustomQuery(p)=>conn.send(
                    &login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer{
                        transaction:p.transaction,payload:None,
                    }).encode()
                ).map_err(err)?,
                login::Clientbound::CookieRequest(p)=>conn.send(
                    &login::Serverbound::CookieResponse(login::CookieResponse{
                        key:p.key,payload:None,
                    }).encode()
                ).map_err(err)?,
                login::Clientbound::Unknown(..)=>{},
            }
        }
        conn.send(&cfg::Serverbound::ClientInformation(cfg::ClientInformation::new(view_distance)).encode()).map_err(err)?;
        let mut w=Writer::new();
        w.string("horizon");
        conn.send(&cfg::Serverbound::CustomPayload(cfg::CustomPayload{
            channel:"minecraft:brand".into(),data:w.data,
        }).encode()).map_err(err)?;
        use ids::configuration::clientbound as cb;
        loop{
            let (id,body)=conn.recv().map_err(err)?;
            // Skip bulky registries and tags: this client does not render a world.
            let answer=match id{
                cb::SELECT_KNOWN_PACKS|cb::KEEP_ALIVE|cb::PING|cb::CODE_OF_CONDUCT
                |cb::COOKIE_REQUEST|cb::DISCONNECT|cb::FINISH_CONFIGURATION=>{
                    match cfg::Clientbound::decode(id,&body).map_err(err)?{
                        cfg::Clientbound::SelectKnownPacks(p)=>cfg::Serverbound::SelectKnownPacks(p),
                        cfg::Clientbound::KeepAlive(p)=>cfg::Serverbound::KeepAlive(p),
                        cfg::Clientbound::Ping(p)=>cfg::Serverbound::Pong(p),
                        cfg::Clientbound::CodeOfConduct(_)=>cfg::Serverbound::AcceptCodeOfConduct(Empty),
                        cfg::Clientbound::CookieRequest(p)=>cfg::Serverbound::CookieResponse(
                            cfg::CookieResponse{key:p.key,payload:None}),
                        cfg::Clientbound::Disconnect(_)=>return Err("disconnected during configuration".into()),
                        cfg::Clientbound::FinishConfiguration(_)=>{
                            conn.send(&cfg::Serverbound::FinishConfiguration(Empty).encode()).map_err(err)?;
                            break;
                        },
                        _=>continue,
                    }
                },
                _=>continue,
            };
            conn.send(&answer.encode()).map_err(err)?;
        }
        Ok(Self{conn,name:name.into(),state:State::default(),
                chunks:HashSet::new(),announced_loaded:false})
    }
    pub fn name(&self)->&str{&self.name}
    pub fn pump_one(&mut self)->Result<(),String>{
        let (id,body)=self.conn.recv().map_err(|e|e.to_string())?;
        self.handle_play_packet(id,&body)
    }
    pub fn handle_play_packet(&mut self,id:i32,body:&[u8])->Result<(),String>{
        use ids::play::clientbound as cb;
        let err=|e:horizon_protocol::Error|e.to_string();
        match id{
            cb::KEEP_ALIVE=>{
                if let play::Clientbound::KeepAlive(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.send(play::Serverbound::KeepAlive(p))?;
                }
            },
            cb::PING=>{
                if let play::Clientbound::Ping(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.send(play::Serverbound::Pong(p))?;
                }
            },
            cb::PLAYER_POSITION=>{
                if let play::Clientbound::PlayerPosition(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    let prev=self.state.position.unwrap_or([0.;3]);
                    let at=std::array::from_fn(|i|if p.relatives&(1<<i)!=0{prev[i]+p.position[i]}else{p.position[i]});
                    let yaw=if p.relatives&(1<<3)!=0{self.state.yaw+p.yaw}else{p.yaw};
                    let pitch=if p.relatives&(1<<4)!=0{self.state.pitch+p.pitch}else{p.pitch};
                    self.state.position=Some(at);
                    self.state.yaw=yaw;
                    self.state.pitch=pitch;
                    self.state.teleports+=1;
                    self.state.ready=false;
                    self.send(play::Serverbound::AcceptTeleportation(play::AcceptTeleportation{
                        id:p.teleport,x:at[0],y:at[1],z:at[2],yaw,pitch,
                    }))?;
                    self.mark_ready()?;
                }
            },
            cb::LEVEL_CHUNK_WITH_LIGHT=>{
                let mut r=Reader::new(body);
                let x=r.i32().map_err(err)?;
                let z=r.i32().map_err(err)?;
                self.chunks.insert((x,z));
                self.state.received_chunks+=1;
                self.mark_ready()?;
            },
            cb::FORGET_LEVEL_CHUNK=>{
                if let play::Clientbound::ForgetLevelChunk(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.chunks.remove(&(p.pos[0],p.pos[1]));
                    self.state.ready=self.state.position.is_some_and(|at|self.has_chunk(at));
                }
            },
            cb::CHUNK_BATCH_FINISHED=>self.send(play::Serverbound::ChunkBatchReceived(
                play::ChunkBatchReceived{chunks_per_tick:4.}
            ))?,
            cb::SET_HEALTH=>{
                if let play::Clientbound::SetHealth(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.state.health=Some(p.health);self.state.hunger=Some(p.food);
                }
            },
            cb::DISCONNECT|cb::START_CONFIGURATION=>
                return Err("client disconnected or requires reconfiguration".into()),
            _=>{},
        }
        Ok(())
    }
    fn has_chunk(&self,at:[f64;3])->bool{
        self.chunks.contains(&((at[0].floor() as i32)>>4,(at[2].floor() as i32)>>4))
    }
    fn mark_ready(&mut self)->Result<(),String>{
        if let Some(at)=self.state.position{
            let loaded=self.has_chunk(at);
            if loaded && !self.announced_loaded{
                self.send(play::Serverbound::PlayerLoaded(Empty))?;
                self.announced_loaded=true;
            }
            self.state.ready=loaded;
        }
        Ok(())
    }
    fn send(&mut self,p:play::Serverbound)->Result<(),String>{
        self.conn.send(&p.encode()).map_err(|e|e.to_string())
    }
    /// At most 0.3 blocks horizontally in a single action. The actual
    /// authoritative server still enforces physics, loaded chunks and reach.
    pub fn walk_delta(&mut self,dx:f64,dz:f64)->Result<[f64;3],String>{
        let at=self.state.position.ok_or("missing spawn")?;
        if !self.state.ready{return Err("current chunk not ready".into());}
        if !dx.is_finite()||!dz.is_finite()||dx*dx+dz*dz>0.09+1e-12{
            return Err("move exceeds a single walking step".into());
        }
        let next=[at[0]+dx,at[1],at[2]+dz];
        if !next.iter().all(|x|x.is_finite())||!self.has_chunk(next){
            return Err("destination chunk unavailable".into());
        }
        self.send(play::Serverbound::MovePlayerPos(play::MovePlayerPos{
            x:next[0],y:next[1],z:next[2],flags:1,
        }))?;
        self.state.position=Some(next);
        Ok(next)
    }
    pub fn use_item_on(&mut self,pos:[i32;3],face:i32,sequence:i32)->Result<(),String>{
        if !self.state.ready{return Err("player not ready".into());}
        if !(0..=5).contains(&face)||sequence<=0{return Err("invalid action".into());}
        self.send(play::Serverbound::UseItemOn(play::UseItemOn{
            hand:0,pos,face,cursor:[0.5;3],inside:false,world_border:false,sequence,
        }))
    }
}
