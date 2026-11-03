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
    srv.join().unwrap();
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
