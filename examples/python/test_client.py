import unittest
from examples.python.client import GraphClient


class Result:
    def __init__(self, count):
        self.count = count

    def single(self):
        return {"processed": self.count}


class Driver:
    def __init__(self, counts=()):
        self.committed, self.counts = [], iter(counts)

    def session(self, *, database):
        self.database = database
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def execute_write(self, callback):
        captured = []
        driver = self

        class Tx:
            def run(self, query, **params):
                captured.append((query, params))
                return Result(next(driver.counts, len(params.get("rows", [1]))))

        result = callback(Tx())
        self.committed.append(captured)
        return result


class ClientTests(unittest.TestCase):
    def test_mixed_shapes_preserve_duplicate_order_and_bounded_batches(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=3)
        rows = [{"entity_id":"same", "status":"first"},
                {"entity_id":"same", "status":"second", "tags":["x"]},
                {"entity_id":"same", "status":"last"},
                {"entity_id":"other", "status":None}]
        self.assertEqual(graph.load_nodes("Ticket", iter(rows)), 4)
        self.assertEqual([len(batch) for batch in driver.committed], [3, 1])
        records = [row for batch in driver.committed for _, params in batch for row in params["rows"]]
        self.assertEqual([row["status"] for row in records], ["first","second","last",None])
        self.assertEqual(driver.database,"tickets")

    def test_patch_separates_unchanged_set_and_remove_and_missing_node(self):
        driver = Driver([1,0])
        graph = GraphClient(driver,"tickets")
        self.assertTrue(graph.update_node("Ticket","t-1",{"status":"open"},remove=["priority"]))
        query, params = driver.committed[0][0]
        self.assertNotIn("MERGE",query)
        self.assertEqual(params,{"id":"t-1","property_0":None,"property_1":"open"})
        self.assertFalse(graph.update_node("Ticket","absent",{"status":"closed"}))

    def test_edge_patch_checks_existence_and_null_removal_in_one_transaction(self):
        driver = Driver([1,1,0])
        graph = GraphClient(driver,"pages")
        self.assertTrue(graph.update_edge(("Page","a"),"LINK",("Page","b"),{"weight":2},remove=["old"]))
        self.assertEqual(len(driver.committed[0]),2)
        self.assertEqual(driver.committed[0][1][1],{"source":"a","target":"b","property_0":None,"property_1":2})
        self.assertFalse(graph.update_edge(("Page","a"),"LINK",("Page","missing"),{"weight":3}))
        self.assertEqual(len(driver.committed[1]),1)
        driver = Driver([0])
        with self.assertRaises(ValueError):
            GraphClient(driver,"pages").upsert_edge(("Page","a"),"LINK",("Page","missing"))
        self.assertEqual(driver.committed,[])

    def test_rejects_injection_reserved_fields_invalid_values_and_oversized_rows(self):
        graph = GraphClient(Driver(),"tickets",batch_bytes=100)
        for label, props in [("Ticket) RETURN 1",{}),("Ticket",{"acl_allow":["user"]}),
                             ("Ticket",{"entity_id":"new"}),("Ticket",{"x":float("nan")}),
                             ("Ticket",{"x":1<<64}),("Ticket",{"x":b"binary"}),
                             ("Ticket",{"x":"x"*101}),("Ticket",[])]:
            with self.subTest(label=label,props=props), self.assertRaises(ValueError):
                graph.upsert_node(label,"id",props)
        with self.assertRaises(ValueError):
            graph.update_node("Ticket","id",{"x":1},remove=["x"])
        self.assertEqual(graph.load_nodes("Ticket",[]),0)


if __name__ == "__main__":
    unittest.main()
