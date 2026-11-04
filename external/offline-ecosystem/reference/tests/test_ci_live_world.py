"""Unit tests for the strict real-world acceptance parser."""
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from ci_live_world import (
    check_events, terrain_report, steering_report, server_mining_report,
    confirm_observed_exploration,
)

def login(name, eid):
    return f"[Server thread/INFO]: {name}[/127.0.0.1:0] logged in with entity id {eid} at (0,64,0)\n"

def survey(name):
    return f"Offline ecosystem {name}: first authoritative survey at 0.50,64.00,0.50\n"

class RealWorldLogTests(unittest.TestCase):
    def test_requires_both_unique_identities_and_sensors(self):
        lines = ["Offline ecosystem: started 2 opt-in local bots\n",
                 login("EcoAI0",2), login("EcoAI1",3),
                 survey("EcoAI0"), survey("EcoAI1")]
        self.assertTrue(check_events(lines)[0])
        self.assertFalse(check_events(lines[:-1])[0])

    def test_rejects_duplicate_server_entity_ids(self):
        lines = ["Offline ecosystem: started 2 opt-in local bots\n",
                 login("EcoAI0",2), login("EcoAI1",2),
                 survey("EcoAI0"), survey("EcoAI1")]
        self.assertFalse(check_events(lines)[0])

    def test_rejects_private_world_gate(self):
        self.assertFalse(check_events(["not_whitelisted"])[0])

    def test_thread_creation_alone_is_not_accepted(self):
        self.assertFalse(check_events(["Offline ecosystem: started 2 opt-in local bots\n"])[0])

class SteeringTelemetryTests(unittest.TestCase):
    def test_counts_packet_emissions_without_inventing_movement(self):
        lines=[
            "Offline ecosystem EcoAI0: steering packets_sent=8 waypoints_reached=0 rejected=2 last_error=chunk unavailable",
            "Offline ecosystem EcoAI0: steering packets_sent=18 waypoints_reached=1 rejected=3 last_error=none",
            "Offline ecosystem EcoAI1: steering packets_sent=0 waypoints_reached=0 rejected=9 last_error=current chunk not ready",
        ]
        report=steering_report(lines)
        self.assertEqual(report["EcoAI0"],{
            "reported":True,"packets_sent":18,"waypoints_reached":1,
            "rejected":3,"last_error":"none",
        })
        self.assertEqual(report["EcoAI1"]["rejected"],9)
        self.assertEqual(steering_report([])["EcoAI0"],{"reported":False})

class MiningDiagnosisTests(unittest.TestCase):
    def test_guards_and_server_stop_outcomes_are_separate_from_harvest(self):
        lines=[
            "Offline ecosystem EcoAI0: server mining START target=[-34, 82, 4] in_reach=false spawn_protected=false inside_border=true spectator=false",
            "Offline ecosystem EcoAI0: server mining START target=[-35, 82, 4] in_reach=true spawn_protected=true inside_border=true spectator=false",
            "Offline ecosystem EcoAI1: server mining START target=[0, 64, 0] in_reach=true spawn_protected=false inside_border=false spectator=false",
            "Offline ecosystem EcoAI0: server mining STOP target=[-35, 82, 4] elapsed_ticks=Some(85) progress_per_tick=0.00500 decision=Delay(12) is_air=false",
            "Offline ecosystem EcoAI0: server mining STOP target=[-35, 82, 4] elapsed_ticks=Some(90) progress_per_tick=0.02000 decision=Break is_air=false",
            "Offline ecosystem EcoAI1: server mining STOP target=[0, 64, 0] elapsed_ticks=None progress_per_tick=0.00000 decision=Other is_air=false",
        ]
        result=server_mining_report(lines)
        self.assertEqual(result["server_starts_observed"],3)
        self.assertEqual(result["starts_out_of_reach"],1)
        self.assertEqual(result["starts_spawn_protected"],1)
        self.assertEqual(result["starts_outside_border"],1)
        self.assertEqual(result["stops_observed"],3)
        self.assertEqual(result["stops_break"],1)
        self.assertEqual(result["stops_delay"],1)
        self.assertEqual(result["stops_other"],1)
        self.assertEqual(server_mining_report([])["server_starts_observed"],0)

class TerrainProbeTests(unittest.TestCase):
    def test_reports_validated_elevations_and_actual_displacement(self):
        lines=[
            "Offline ecosystem EcoAI0: first authoritative survey at -29.50,68.00,1.50 logs=0 ground_tiles=2 terrain_down=17 terrain_up=9 current_ground=false",
            "Offline ecosystem EcoAI0: exploration survey position -27.50,67.00,1.50 logs=0 ground_tiles=5 terrain_down=11 terrain_up=8 current_ground=true wood_inventory=0",
            "Offline ecosystem EcoAI1: first authoritative survey at -31.50,67.00,-1.50 logs=0 ground_tiles=3 terrain_down=2 terrain_up=0 current_ground=true",
        ]
        result=terrain_report(lines)
        self.assertEqual(result["EcoAI0"]["samples"],2)
        self.assertEqual(result["EcoAI0"]["horizontal_displacement"],2.0)
        self.assertEqual(result["EcoAI0"]["max_sampled_excursion"],2.0)
        self.assertEqual(result["EcoAI0"]["sampled_path_length"],2.0)
        self.assertEqual(result["EcoAI0"]["min_sampled_y"],67.0)
        self.assertEqual(result["EcoAI0"]["max_sampled_y"],68.0)
        self.assertEqual(result["EcoAI0"]["first_lower_stands"],17)
        self.assertEqual(result["EcoAI0"]["first_higher_stands"],9)
        self.assertFalse(result["EcoAI0"]["initial_column_standable"])
        self.assertEqual(result["EcoAI1"]["horizontal_displacement"],0)

    def test_round_trip_reports_observed_exploration_even_at_spawn(self):
        lines=[
            "Offline ecosystem EcoAI0: first authoritative survey at 0.50,64.00,0.50 logs=0 ground_tiles=2 terrain_down=0 terrain_up=2 current_ground=true",
            "Offline ecosystem EcoAI0: exploration survey position 2.50,64.00,0.50 logs=0 ground_tiles=4 terrain_down=0 terrain_up=2 current_ground=true",
            "Offline ecosystem EcoAI0: exploration survey position 0.50,64.00,0.50 logs=0 ground_tiles=3 terrain_down=0 terrain_up=2 current_ground=true",
        ]
        result=terrain_report(lines)["EcoAI0"]
        self.assertEqual(result["horizontal_displacement"],0.0)
        self.assertEqual(result["max_sampled_excursion"],2.0)
        self.assertEqual(result["sampled_path_length"],4.0)
        self.assertEqual(result["samples"],3)

    def test_strict_movement_gate_excludes_login_only_and_packet_attempts(self):
        only_login=[
            "Offline ecosystem: started 2 opt-in local bots",
            login("EcoAI0",2), login("EcoAI1",3),
            survey("EcoAI0"), survey("EcoAI1"),
            "Offline ecosystem EcoAI0: steering packets_sent=999 waypoints_reached=0 rejected=0 last_error=none",
        ]
        ok,detail=confirm_observed_exploration(only_login)
        self.assertFalse(ok)
        self.assertIn("0/2",detail)
        progress=[
            "Offline ecosystem EcoAI0: first authoritative survey at -2.50,64.00,1.50 logs=0 ground_tiles=4 terrain_down=0 terrain_up=2 current_ground=true",
            "Offline ecosystem EcoAI0: exploration survey position 0.50,64.00,1.50 logs=0 ground_tiles=6 terrain_down=0 terrain_up=2 current_ground=true",
        ]
        self.assertTrue(confirm_observed_exploration(progress)[0])
        self.assertFalse(confirm_observed_exploration(progress,min_bots=2)[0])

    def test_does_not_infer_unreported_movement(self):
        self.assertEqual(terrain_report([])["EcoAI0"],{"samples":0,"horizontal_displacement":0.0})


if __name__ == "__main__":
    unittest.main()
