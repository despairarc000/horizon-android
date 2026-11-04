"""Unit tests for the strict real-world acceptance parser."""
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from ci_live_world import check_events

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

if __name__ == "__main__":
    unittest.main()
