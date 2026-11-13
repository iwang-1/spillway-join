import random
import subprocess
import tempfile
import unittest
from pathlib import Path

from experiments.verify import BIN, verify


class JoinTests(unittest.TestCase):
    def test_seeded_multisets(self):
        for seed in range(32):
            rng = random.Random(seed)
            def relation():
                return [(rng.randint(-8,8),rng.randint(-2,2)) for _ in range(rng.randrange(60))]
            with self.subTest(seed=seed):
                verify(relation(), relation(), rng.choice([1,3,8,64]))

    def test_hot_key(self):
        metrics = verify([(1,1)]*35, [(1,-1)]*28, 4)
        self.assertEqual(metrics["output_rows"], 980)
        self.assertEqual(metrics["fallback_pairs"], 1)
        self.assertEqual(metrics["probe_rows_scanned"], 35 * 7)

    def test_limits_and_empty(self):
        data = [(-(2**63),2**63-1), (2**63-1,0)]
        self.assertEqual(verify([], data, 1)["output_rows"], 0)
        self.assertEqual(verify(data, data, 1)["output_rows"], 2)

    def test_publish_and_cleanup(self):
        with tempfile.TemporaryDirectory() as tmp:
            l,r,out = [Path(tmp)/name for name in ("l","r","out")]
            l.write_text("1\t4\n")
            r.write_text("2\tbad\n")
            args = [BIN,l,r,out,"1"]
            self.assertNotEqual(subprocess.run(args,capture_output=True).returncode,0)
            self.assertFalse(out.exists())
            self.assertEqual(sorted(p.name for p in Path(tmp).iterdir()), ["l","r"])
            r.write_text("1\t7\n")
            workers = [subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE) for _ in range(4)]
            for worker in workers:
                worker.communicate()
            self.assertEqual(sorted(w.returncode for w in workers), [0,1,1,1])
            self.assertEqual(out.read_text(),"1\t4\t7\n")
