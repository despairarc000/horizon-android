"""Unit tests for the strict real-world acceptance parser."""
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from ci_live_world import check_events,terrain_report

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
        self.assertEqual(result["EcoAI0"]["first_lower_stands"],17)
        self.assertEqual(result["EcoAI0"]["first_higher_stands"],9)
        self.assertFalse(result["EcoAI0"]["initial_column_standable"])
        self.assertEqual(result["EcoAI1"]["horizontal_displacement"],0)

    def test_does_not_infer_unreported_movement(self):
        self.assertEqual(terrain_report([])["EcoAI0"],{"samples":0,"horizontal_displacement":0.0})


if __name__ == "__main__":
    unittest.main()
