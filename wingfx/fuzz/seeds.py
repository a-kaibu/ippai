# Seeds for fuzz/run.sh: the case files cut into pieces of a few cases each (seeds/cases/).
import os
here = os.path.dirname(os.path.abspath(__file__))
out = os.path.join(here, "seeds", "cases")
os.makedirs(out, exist_ok=True)
for name in sorted(os.listdir(os.path.join(here, "..", "cases"))):
    text = open(os.path.join(here, "..", "cases", name)).read()
    parts = text.split("\ncase ")
    for i in range(0, len(parts), 25):
        open(os.path.join(out, f"{name}-{i}"), "w").write("case " + "\ncase ".join(parts[i:i + 3]))
