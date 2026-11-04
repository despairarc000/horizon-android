use std::thread;
use horizon_offline_ecosystem_client::LocalPlayer;
use horizon_protocol::{
    PROTOCOL_VERSION,buf::Writer,
    connection::{Connection,Pipe},
    crypto::offline_uuid,ids,
    packets::{Empty,configuration as cfg,handshake,login,play},
};

fn mock_server(mut s:Connection)->Connection{
    let (id,body)=s.recv().unwrap();
    let handshake::Serverbound::Intention(h)=handshake::Serverbound::decode(id,&body).unwrap() else{panic!("handshake")};
    assert_eq!(h.protocol,PROTOCOL_VERSION);
    assert_eq!(h.intent,handshake::LOGIN);
    let (id,body)=s.recv().unwrap();
    let login::Serverbound::Hello(h)=login::Serverbound::decode(id,&body).unwrap() else{panic!("login")};
    s.send(&login::Clientbound::LoginFinished(login::LoginFinished{
        profile:login::GameProfile{uuid:offline_uuid(&h.name),name:h.name,properties:vec![]},
        session:1,
    }).encode()).unwrap();
    let (id,body)=s.recv().unwrap();
    assert!(matches!(login::Serverbound::decode(id,&body).unwrap(),login::Serverbound::LoginAcknowledged(_)));
    let (id,body)=s.recv().unwrap();
    assert!(matches!(cfg::Serverbound::decode(id,&body).unwrap(),cfg::Serverbound::ClientInformation(_)));
    let (id,body)=s.recv().unwrap();
    assert!(matches!(cfg::Serverbound::decode(id,&body).unwrap(),cfg::Serverbound::CustomPayload(_)));
    s.send(&cfg::Clientbound::SelectKnownPacks(cfg::SelectKnownPacks{packs:vec![]}).encode()).unwrap();
    let (id,body)=s.recv().unwrap();
    assert!(matches!(cfg::Serverbound::decode(id,&body).unwrap(),cfg::Serverbound::SelectKnownPacks(_)));
    s.send(&cfg::Clientbound::KeepAlive(cfg::KeepAlive{id:42}).encode()).unwrap();
    let (id,body)=s.recv().unwrap();
    assert!(matches!(cfg::Serverbound::decode(id,&body).unwrap(),cfg::Serverbound::KeepAlive(cfg::KeepAlive{id:42})));
    s.send(&cfg::Clientbound::FinishConfiguration(Empty).encode()).unwrap();
    let (id,body)=s.recv().unwrap();
    assert!(matches!(cfg::Serverbound::decode(id,&body).unwrap(),cfg::Serverbound::FinishConfiguration(_)));
    s
}

#[test]
fn in_memory_join_teleport_loaded_chunk_move_and_use() {
    let (client,server)=Pipe::pair();
    let srv=thread::spawn(move||{
        let mut s=mock_server(Connection::local(server));
        s.send(&play::Clientbound::PlayerPosition(play::PlayerPosition{
            teleport:3,position:[1.5,65.,1.5],delta:[0.;3],
            yaw:0.,pitch:0.,relatives:0,
        }).encode()).unwrap();
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::AcceptTeleportation(t)=play::Serverbound::decode(id,&body).unwrap() else{panic!("teleport ack")};
        assert_eq!(t.id,3);
        s.send(&play::Clientbound::KeepAlive(cfg::KeepAlive{id:555}).encode()).unwrap();
        let (id,body)=s.recv().unwrap();
        assert!(matches!(play::Serverbound::decode(id,&body).unwrap(),play::Serverbound::KeepAlive(cfg::KeepAlive{id:555})));
        // Only a chunk's x/z header is necessary for availability tracking.
        let mut raw=Writer::new();
        raw.varint(ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
        raw.i32(0);
        raw.i32(0);
        s.send(&raw.data).unwrap();
        let (id,body)=s.recv().unwrap();
        assert!(matches!(play::Serverbound::decode(id,&body).unwrap(),play::Serverbound::PlayerLoaded(_)));
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::MovePlayerPos(m)=play::Serverbound::decode(id,&body).unwrap() else{panic!("move")};
        assert!((m.x-1.7).abs()<1e-5);
        assert!((m.z-1.6).abs()<1e-5);
        let (id,body)=s.recv().unwrap();
        assert!(matches!(play::Serverbound::decode(id,&body).unwrap(),play::Serverbound::UseItemOn(_)));
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::PlayerAction(start)=play::Serverbound::decode(id,&body).unwrap() else{panic!("mining start")};
        assert_eq!((start.action,start.sequence),(0,12));
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::PlayerAction(stop)=play::Serverbound::decode(id,&body).unwrap() else{panic!("mining stop")};
        assert_eq!((stop.action,stop.sequence),(3,13));
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::Attack(attack)=play::Serverbound::decode(id,&body).unwrap() else{panic!("attack")};
        assert_eq!(attack.id,101);
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::SetCarriedItem(selected)=play::Serverbound::decode(id,&body).unwrap() else{panic!("hotbar selection");};
        assert_eq!(selected.slot,0);
        let (id,body)=s.recv().unwrap();
        let play::Serverbound::UseItem(use_request)=play::Serverbound::decode(id,&body).unwrap() else{panic!("item use");};
        assert_eq!((use_request.hand,use_request.sequence),(0,104));
    });
    let mut bot=LocalPlayer::join(client,"EcoBot0",2).unwrap();
    assert_eq!(bot.name(),"EcoBot0");
    bot.pump_one().unwrap();
    assert!(!bot.state.ready);
    assert!(bot.walk_delta(0.1,0.).is_err());
    bot.pump_one().unwrap();
    bot.pump_one().unwrap();
    assert!(bot.state.ready);
    assert!(bot.walk_delta(1.,0.).is_err());
    assert!(bot.walk_delta(f64::NAN,0.).is_err());
    bot.walk_delta(0.2,0.1).unwrap();
    bot.use_item_on([1,64,1],1,1).unwrap();
    assert!(bot.begin_mining([1,64,1],6,11).is_err());
    assert!(bot.attack_entity(0).is_err());
    bot.begin_mining([1,64,1],1,12).unwrap();
    bot.finish_mining([1,64,1],1,13).unwrap();
    bot.attack_entity(101).unwrap();
    assert!(bot.select_hotbar(9).is_err());
    assert!(bot.use_selected_item(0).is_err());
    bot.select_hotbar(0).unwrap();
    bot.use_selected_item(104).unwrap();
    srv.join().unwrap();
}

/// A synthetic in-memory protocol peer verifies honest inventory-menu
/// synchronization. This does NOT pretend that the real Minecraft crafting
/// rules ran; a separate resource-backed survival test is still required.
#[test]
fn inventory_click_requires_real_server_snapshot_and_reconciliation() {
    use horizon_protocol::item::Stack;
    let (client,server)=Pipe::pair();
    let peer=thread::spawn(move||{
        let mut server=mock_server(Connection::local(server));
        server.send(&play::Clientbound::PlayerPosition(play::PlayerPosition{
            teleport:5,position:[1.5,65.,1.5],delta:[0.;3],
            yaw:0.,pitch:0.,relatives:0,
        }).encode()).unwrap();
        let (id,body)=server.recv().unwrap();
        assert!(matches!(
            play::Serverbound::decode(id,&body).unwrap(),
            play::Serverbound::AcceptTeleportation(_)
        ));
        let mut chunk=Writer::new();
        chunk.varint(ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
        chunk.i32(0);chunk.i32(0);
        server.send(&chunk.data).unwrap();
        let (id,body)=server.recv().unwrap();
        assert!(matches!(
            play::Serverbound::decode(id,&body).unwrap(),
            play::Serverbound::PlayerLoaded(_)
        ));
        let mut slots=vec![None;46];
        slots[36]=Some(Stack::new(7,2));
        server.send(&play::Clientbound::ContainerSetContent(
            play::ContainerSetContent{
                container:0,state:4,items:slots,carried:None,
            }
        ).encode()).unwrap();
        let (id,body)=server.recv().unwrap();
        let play::Serverbound::ContainerClick(click)=
            play::Serverbound::decode(id,&body).unwrap()
        else {panic!("expected ordinary inventory click");};
        assert_eq!((click.container,click.state,click.slot,click.button,click.input),
                   (0,4,36,0,0));
        // A real authoritative response clears the pending click; the
        // client never credits or transfers an item optimistically.
        server.send(&play::Clientbound::ContainerSetSlot(
            play::ContainerSetSlot{
                container:0,state:5,slot:36,item:None,
            }
        ).encode()).unwrap();
    });
    let mut bot=LocalPlayer::join(client,"EcoMenu",2).unwrap();
    assert!(bot.inventory_click(36,0,0).is_err());
    bot.pump_one().unwrap();  // real teleport
    bot.pump_one().unwrap();  // loaded chunk
    assert!(bot.state.ready);
    assert!(bot.inventory_click(36,0,0).is_err());
    bot.pump_one().unwrap();  // authoritative inventory snapshot
    let menu=bot.inventory_menu().unwrap();
    assert_eq!(menu.revision,4);
    assert_eq!(menu.slots.len(),46);
    assert_eq!(menu.slots[36].as_ref().unwrap().count,2);
    assert!(bot.inventory_click(0,0,3).is_err()); // creative clone forbidden
    assert!(bot.inventory_click(-1,0,0).is_err());
    bot.inventory_click(36,0,0).unwrap();
    assert!(bot.inventory_click_pending());
    assert!(bot.inventory_click(1,0,0).is_err());
    bot.pump_one().unwrap();
    assert!(!bot.inventory_click_pending());
    assert_eq!(bot.inventory_menu().unwrap().revision,5);
    assert!(bot.inventory_menu().unwrap().slots[36].is_none());
    peer.join().unwrap();
}

#[test]
fn invalid_name_is_rejected_without_blocking() {
    let (client,_server)=Pipe::pair();
    assert!(LocalPlayer::join(client,"Bad Bot",2).is_err());
}

#[test]
fn fingerprint_challenge_on_local_connection(){
    let (client,server)=Pipe::pair();
    let server=thread::spawn(move||{
        let mut s=Connection::local(server);
        let _=s.recv().unwrap(); // handshake
        let (id,body)=s.recv().unwrap();
        let login::Serverbound::Hello(h)=login::Serverbound::decode(id,&body).unwrap() else{panic!("hello");};
        s.send(&login::Clientbound::LoginFinished(login::LoginFinished{
            profile:login::GameProfile{uuid:offline_uuid(&h.name),name:h.name,properties:vec![]},session:123,
        }).encode()).unwrap();
        let _=s.recv().unwrap(); // login ack
        let _=s.recv().unwrap(); // client information
        let _=s.recv().unwrap(); // brand
        let fingerprint=vec![42;32];
        s.send(&cfg::Clientbound::SelectKnownPacks(cfg::SelectKnownPacks{packs:vec![]}).encode()).unwrap();
        let _=s.recv().unwrap();
        s.send(&cfg::Clientbound::CustomPayload(cfg::CustomPayload{channel:"horizon:block_registry".into(),data:fingerprint.clone()}).encode()).unwrap();
        s.send(&cfg::Clientbound::Ping(cfg::Ping{id:0}).encode()).unwrap();
        let (id,body)=s.recv().unwrap();
        let cfg::Serverbound::CustomPayload(reply)=cfg::Serverbound::decode(id,&body).unwrap() else{panic!("missing fingerprint response");};
        assert_eq!(reply.channel,"horizon:block_registry");
        assert_eq!(reply.data,fingerprint);
        let (id,body)=s.recv().unwrap();
        assert!(matches!(cfg::Serverbound::decode(id,&body).unwrap(),cfg::Serverbound::Pong(cfg::Ping{id:0})));
        s.send(&cfg::Clientbound::FinishConfiguration(Empty).encode()).unwrap();
        let _=s.recv().unwrap();
    });
    let bot=LocalPlayer::join(client,"EcoHash",2).unwrap();
    assert_eq!(bot.name(),"EcoHash");
    server.join().unwrap();
}

#[test]
fn time_bounded_poll_allows_autonomous_decisions(){
    use std::time::Duration;
    let (client,server)=Pipe::pair();
    let peer=thread::spawn(move||{
        let _server=mock_server(Connection::local(server));
        thread::sleep(Duration::from_millis(75));
    });
    let mut bot=LocalPlayer::join(client,"EcoClock",2).unwrap();
    assert!(!bot.poll_one_timeout(Duration::from_millis(10)).unwrap());
    assert!(bot.poll_one_timeout(Duration::ZERO).is_err());
    assert!(bot.poll_one_timeout(Duration::from_secs(3)).is_err());
    assert!(bot.steer_xz([1.,1.]).is_err());
    peer.join().unwrap();
}
