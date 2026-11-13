"""Compare every emitted triple, including multiplicity, with SQLite."""
import argparse
import json
import random
import sqlite3
import subprocess
import tempfile
from pathlib import Path

BIN = Path(__file__).resolve().parents[1] / "target/release/spillway-join"


def write_rows(path, rows):
    with path.open("w") as stream:
        for key, value in rows:
            stream.write(f"{key}\t{value}\n")


def verify(left, right, budget):
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        l, r, out = [root / n for n in ("left", "right", "out")]
        write_rows(l, left)
        write_rows(r, right)
        result = subprocess.run([BIN, l, r, out, str(budget)], capture_output=True, check=True)
        metrics = json.loads(result.stdout)
        db = sqlite3.connect(root / "oracle.db")
        try:
            for name, rows in (("l", left), ("r", right)):
                db.execute(f"CREATE TABLE {name}(k INTEGER, v INTEGER)")
                db.executemany(f"INSERT INTO {name} VALUES(?,?)", rows)
            db.execute("CREATE TABLE observed(k INTEGER,a INTEGER,b INTEGER)")
            with out.open() as stream:
                db.executemany("INSERT INTO observed VALUES(?,?,?)",
                               (tuple(map(int,line.split())) for line in stream))
            expected = db.execute("SELECT l.k,l.v,r.v FROM l JOIN r ON l.k=r.k ORDER BY 1,2,3")
            actual = db.execute("SELECT * FROM observed ORDER BY 1,2,3")
            count = 0
            while True:
                a, b = expected.fetchmany(8192), actual.fetchmany(8192)
                if a != b:
                    raise AssertionError(f"multiset mismatch after {count} rows")
                count += len(a)
                if not a:
                    break
            if count != metrics["output_rows"] or metrics["peak_build_rows"] > budget:
                raise AssertionError("incorrect operator counters")
        finally:
            db.close()
        return metrics


def corpus(n, seed, skew):
    rng = random.Random(seed)
    for i in range(n):
        key = -1 if skew and i < 800 else rng.randrange(n * 2)
        yield key, rng.randrange(10000)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rows", type=int, default=100000)
    args = parser.parse_args()
    report = []
    for skew in (False, True):
        left = list(corpus(args.rows, 17, skew))
        right = list(corpus(args.rows, 23, skew))
        for budget in (256, 4096):
            report.append(dict(workload="hot-key" if skew else "uniform", budget=budget,
                               **verify(left, right, budget)))
    print(json.dumps(report, indent=2))
