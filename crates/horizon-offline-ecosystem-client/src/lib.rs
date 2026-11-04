//! Offline, headless player protocol over an in-memory Pipe.
//! The caller runs the authoritative Minecraft integrated server concurrently.
//! Nothing here is a network service, Minecraft AI, or a replacement for server rules.

use std::collections::HashSet;
use horizon_protocol::{
    item::Stack,
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
    /// A block-use acknowledgement from Horizon; does not alone prove
    /// placement succeeded (inspect the corresponding BlockUpdate).
    pub last_block_ack: Option<i32>,
    pub last_block_update: Option<([i32;3],i32)>,
}

/// A server-authoritative snapshot of the 46-slot 2x2 inventory menu.
///
/// The bot never creates an item locally. A snapshot can only be populated by
/// an actual Horizon ContainerSetContent packet and updated by its slot
/// corrections. The server's own 2x2 crafting engine remains authoritative.
#[derive(Clone,Debug,PartialEq)]
pub struct InventoryMenu {
    pub revision:i32,
    pub slots:Vec<Option<Stack>>,
    pub carried:Option<Stack>,
}

pub struct LocalPlayer {
    conn: Connection,
    name: String,
    pub state: State,
    chunks: HashSet<(i32,i32)>,
    announced_loaded: bool,
    inventory_menu:Option<InventoryMenu>,
    menu_click_pending:bool,
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
        conn.send(&cfg::Serverbound::ClientInformation(cfg::ClientInformation::new(view_distance as i8)).encode()).map_err(err)?;
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
                cb::CUSTOM_PAYLOAD=>{
                    // The in-process client shares Horizon's content registry.
                    // Echo the server's fingerprint strictly for an in-memory
                    // connection. This is NOT proof of compatibility for a
                    // remote client and must never be used for arbitrary TCP.
                    let packet=cfg::Clientbound::decode(id,&body).map_err(err)?;
                    if let cfg::Clientbound::CustomPayload(p)=packet{
                        if p.channel=="horizon:block_registry"{
                            conn.send(&cfg::Serverbound::CustomPayload(p).encode()).map_err(err)?;
                        }
                    }
                    continue;
                },
                _=>continue,
            };
            conn.send(&answer.encode()).map_err(err)?;
        }
        Ok(Self{conn,name:name.into(),state:State::default(),
                chunks:HashSet::new(),announced_loaded:false,
                inventory_menu:None,menu_click_pending:false})
    }
    pub fn name(&self)->&str{&self.name}
    pub fn inventory_menu(&self)->Option<&InventoryMenu>{
        self.inventory_menu.as_ref()
    }
    pub fn inventory_click_pending(&self)->bool{self.menu_click_pending}
    /// Poll the local connection for one packet without starving the bot
    /// decision loop when the server sends no packets. The read timeout is
    /// reset so the original blocking pump retains its meaning.
    pub fn poll_one_timeout(&mut self,wait:std::time::Duration)->Result<bool,String>{
        if wait.is_zero() || wait>std::time::Duration::from_millis(250){
            return Err("poll timeout must be 1..250 ms".into());
        }
        self.conn.reader.set_read_timeout(Some(wait)).map_err(|e|e.to_string())?;
        let read=self.conn.recv();
        self.conn.reader.set_read_timeout(None).map_err(|e|e.to_string())?;
        match read{
            Ok((id,body))=>{self.handle_play_packet(id,&body)?;Ok(true)},
            Err(e) if e.is_timed_out()=>Ok(false),
            Err(e)=>Err(e.to_string()),
        }
    }
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
            cb::BLOCK_CHANGED_ACK=>{
                if let play::Clientbound::BlockChangedAck(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.state.last_block_ack=Some(p.sequence);
                }
            },
            cb::BLOCK_UPDATE=>{
                if let play::Clientbound::BlockUpdate(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.state.last_block_update=Some((p.pos,p.state));
                }
            },
            cb::SET_HEALTH=>{
                if let play::Clientbound::SetHealth(p)=play::Clientbound::decode(id,body).map_err(err)?{
                    self.state.health=Some(p.health);self.state.hunger=Some(p.food);
                }
            },
            cb::CONTAINER_SET_CONTENT=>{
                // The 2x2 inventory menu is window zero and exactly 46
                // slots. Other windows and incomplete snapshots cannot
                // authorize a crafting click.
                if let Ok(play::Clientbound::ContainerSetContent(p))=
                    play::Clientbound::decode(id,body)
                {
                    if p.container==0 {
                        self.inventory_menu=if p.items.len()==46 {
                            Some(InventoryMenu{
                                revision:p.state,slots:p.items,carried:p.carried
                            })
                        } else {None};
                        self.menu_click_pending=false;
                    }
                } else {
                    // Unknown component encoding? Keep playing, but disable
                    // all inventory actions until a valid full snapshot.
                    self.inventory_menu=None;
                    self.menu_click_pending=false;
                }
            },
            cb::CONTAINER_SET_SLOT=>{
                if let Ok(play::Clientbound::ContainerSetSlot(p))=
                    play::Clientbound::decode(id,body)
                {
                    if p.container==0{
                        if let Some(menu)=self.inventory_menu.as_mut(){
                            if let Ok(i)=usize::try_from(p.slot) {
                                if i<menu.slots.len(){
                                    menu.slots[i]=p.item;
                                    menu.revision=p.state;
                                    self.menu_click_pending=false;
                                }
                            }
                        }
                    }
                } else {
                    self.inventory_menu=None;
                    self.menu_click_pending=false;
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
    /// Send a single small downward movement using normal survival player
    /// packets. This does not cause a teleport or invent physics, collision
    /// or landing; the server remains the final authority.
    /// Only a confirmed 3D surface route may request this action.
    pub fn descend_delta(&mut self,dx:f64,dy:f64,dz:f64,on_ground:bool)
        ->Result<[f64;3],String>
    {
        let at=self.state.position.ok_or("missing spawn")?;
        if !self.state.ready{return Err("current chunk not ready".into());}
        if ![dx,dy,dz].iter().all(|v|v.is_finite())
            || dx*dx+dz*dz>0.040001
            || !(-0.300001..0.0).contains(&dy)
        {
            return Err("descent packet exceeds bounded survival movement".into());
        }
        let next=[at[0]+dx,at[1]+dy,at[2]+dz];
        if !next.iter().all(|v|v.is_finite()) || !self.has_chunk(next) {
            return Err("descent destination chunk unavailable".into());
        }
        self.send(play::Serverbound::MovePlayerPos(play::MovePlayerPos{
            x:next[0],y:next[1],z:next[2],flags:u8::from(on_ground)
        }))?;
        // This is only a local prediction. The next server survey will
        // reconcile physical position and can disprove the requested landing.
        self.state.position=Some(next);
        Ok(next)
    }
    /// Send a single bounded normal survival jump/fall movement packet.
    /// Never teleport, fly, synthesize collision clearance or set inventory.
    /// The first upward packet has on_ground=false so Horizon performs its
    /// actual jump-from-ground handling and validates the resulting motion.
    pub fn jump_delta(
        &mut self,dx:f64,dy:f64,dz:f64,on_ground:bool
    )->Result<[f64;3],String>{
        let at=self.state.position.ok_or("missing spawn")?;
        if !self.state.ready{return Err("current chunk not ready".into());}
        if ![dx,dy,dz].iter().all(|v|v.is_finite())
            || dx*dx+dz*dz>0.057601
            || !(-0.300001..=0.420001).contains(&dy)
            || (dy>0.0 && on_ground)
        {
            return Err("jump packet exceeds bounded survival movement".into());
        }
        let next=[at[0]+dx,at[1]+dy,at[2]+dz];
        if !next.iter().all(|v|v.is_finite()) || !self.has_chunk(next){
            return Err("jump destination chunk unavailable".into());
        }
        self.send(play::Serverbound::MovePlayerPos(play::MovePlayerPos{
            x:next[0],y:next[1],z:next[2],flags:u8::from(on_ground),
        }))?;
        // Local prediction is not an observed jump. Authoritative surveys
        // overwrite this value and can reject the entire attempted arc.
        self.state.position=Some(next);
        Ok(next)
    }
    /// One conservative horizontal steering action toward a known target.
    /// This is a low-level action, not pathfinding. It cannot cross obstacles,
    /// climb, jump or validate a survival task, and may be corrected by server.
    pub fn steer_xz(&mut self,target:[f64;2])->Result<bool,String>{
        let at=self.state.position.ok_or("missing spawn")?;
        if !target.iter().all(|x|x.is_finite()){
            return Err("invalid steering target".into());
        }
        let dx=target[0]-at[0];
        let dz=target[1]-at[2];
        let dist=dx.hypot(dz);
        if !dist.is_finite(){return Err("invalid steering distance".into());}
        if dist<=0.02{return Ok(true);}
        let step=dist.min(0.2);
        self.walk_delta(dx/dist*step,dz/dist*step)?;
        Ok(false)
    }
    /// Submit one ordinary, 2x2 inventory-menu click. Never predict an
    /// inventory mutation or call it a craft: only a subsequent full server
    /// menu snapshot or slot update can acknowledge the action. Disallow
    /// creative clone, throw and unsupported drag inputs.
    pub fn inventory_click(&mut self,slot:i16,button:i8,input:i32)
        ->Result<(),String>
    {
        if !self.state.ready{return Err("player not ready".into());}
        let menu=self.inventory_menu.as_ref()
            .ok_or("server inventory menu has not been observed")?;
        if self.menu_click_pending {
            return Err("previous inventory click not yet synchronized".into());
        }
        if !(0..=44).contains(&slot)
            || !matches!((input,button),(0,0)|(0,1)|(1,0)|(1,1))
        {
            return Err("unsupported inventory click mode or slot".into());
        }
        let state=menu.revision;
        self.send(play::Serverbound::ContainerClick(play::ContainerClick{
            container:0,state,slot,button,input,changed:Vec::new(),carried:None,
        }))?;
        self.menu_click_pending=true;
        Ok(())
    }
    /// Start or stop mining using normal Minecraft player packets. Vanilla
    /// tool speed and mining time must be respected by a future task planner.
    fn mining(&mut self,kind:i32,pos:[i32;3],face:u8,sequence:i32)->Result<(),String>{
        if !self.state.ready{return Err("player not ready".into());}
        if face>5||sequence<=0{return Err("invalid mining action".into());}
        self.send(play::Serverbound::PlayerAction(play::PlayerAction{
            action:kind,pos,direction:face,sequence,
        }))
    }
    pub fn begin_mining(&mut self,pos:[i32;3],face:u8,sequence:i32)->Result<(),String>{
        self.mining(0,pos,face,sequence)
    }
    pub fn abort_mining(&mut self,pos:[i32;3],face:u8,sequence:i32)->Result<(),String>{
        self.mining(2,pos,face,sequence)
    }
    pub fn finish_mining(&mut self,pos:[i32;3],face:u8,sequence:i32)->Result<(),String>{
        self.mining(3,pos,face,sequence)
    }
    /// The server verifies attack range, damage and cooldown. No local
    /// assumption of a kill or successful hit is allowed.
    pub fn attack_entity(&mut self,id:i32)->Result<(),String>{
        if !self.state.ready||id<=0{return Err("invalid attack".into());}
        self.send(play::Serverbound::Attack(play::Attack{id}))
    }
    /// Mark the end of one client simulation tick. Vanilla sends this after
    /// its tick to let the server safely schedule movement and interactions.
    pub fn client_tick_end(&mut self)->Result<(),String>{
        self.send(play::Serverbound::ClientTickEnd(Empty))
    }
    /// Select an actual hotbar slot, which the authoritative server may
    /// accept or reject. This never manufactures inventory.
    pub fn select_hotbar(&mut self,slot:u8)->Result<(),String>{
        if !self.state.ready{return Err("player not ready".into());}
        if slot>8{return Err("hotbar slot must be 0..=8".into());}
        self.send(play::Serverbound::SetCarriedItem(play::SetCarriedItem{
            slot:i16::from(slot),
        }))
    }
    /// Begin using the selected item under normal Minecraft survival rules.
    /// Only server inventory changes prove actual food consumption.
    pub fn use_selected_item(&mut self,sequence:i32)->Result<(),String>{
        if !self.state.ready||sequence<=0{return Err("invalid item-use request".into());}
        self.send(play::Serverbound::UseItem(play::UseItem{
            hand:0,sequence,yaw:self.state.yaw,pitch:self.state.pitch,
        }))
    }
    pub fn use_item_on(&mut self,pos:[i32;3],face:i32,sequence:i32)->Result<(),String>{
        if !self.state.ready{return Err("player not ready".into());}
        if !(0..=5).contains(&face)||sequence<=0{return Err("invalid action".into());}
        self.send(play::Serverbound::UseItemOn(play::UseItemOn{
            hand:0,pos,face,cursor:[0.5;3],inside:false,world_border:false,sequence,
        }))
    }
}
