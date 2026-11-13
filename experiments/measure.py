"""Measure each join in a fresh child process using the same TSV input and output contract."""
import json
import os
import platform
import resource
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from verify import BIN, corpus, write_rows


def sqlite_join(left, right, out, database):
    db = sqlite3.connect(database)
    try:
        for name, path in (("l", left), ("r", right)):
            db.execute(f"CREATE TABLE {name}(k INTEGER, v INTEGER)")
            with open(path) as f:
                db.executemany(f"INSERT INTO {name} VALUES(?,?)", (line.split() for line in f))
        db.commit()
        with open(out, "x") as f:
            for key, a, b in db.execute("SELECT l.k,l.v,r.v FROM l JOIN r ON l.k=r.k"):
                f.write(f"{key}\t{a}\t{b}\n")
    finally:
        db.close()


def measure(cmd):
    start = time.perf_counter()
    child = subprocess.Popen(cmd, stdout=subprocess.DEVNULL)
    _, status, usage = os.wait4(child.pid, 0)
    child.returncode = os.waitstatus_to_exitcode(status)
    if child.returncode:
        raise RuntimeError(f"child exited with {child.returncode}")
    rss_scale = 1 if sys.platform == "darwin" else 1024
    return {"seconds": round(time.perf_counter()-start, 4), "peak_rss_bytes": usage.ru_maxrss * rss_scale}


def benchmark():
    report = {"os": platform.system(), "architecture": platform.machine(), "python": platform.python_version(),
              "sqlite": sqlite3.sqlite_version, "rows_per_input": 100000, "runs": []}
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        for skew in (False, True):
            paths = [root/"left", root/"right"]
            for path, seed in zip(paths, (17,23)):
                write_rows(path, corpus(100000, seed, skew))
            runs = {s: [] for s in ("spillway-256","spillway-4096","sqlite-python")}
            for trial in range(3):
                names = list(runs)
                for name in names[trial:] + names[:trial]:
                    with tempfile.TemporaryDirectory(dir=root) as run:
                        out = Path(run)/"out"
                        cmd = ([sys.executable, __file__, "sqlite", *paths, out, Path(run)/"db"] if name == "sqlite-python"
                               else [BIN, *paths, out, name.split("-")[1]])
                        runs[name].append(measure(cmd))
            for name, measurements in runs.items():
                report["runs"].append({"workload":"hot-key" if skew else "uniform","engine":name,
                                       "trials":measurements,"median_seconds":statistics.median(x["seconds"] for x in measurements)})
    return report


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "sqlite":
        sqlite_join(*sys.argv[2:])
    else:
        print(json.dumps(benchmark(), indent=2))
