from loader import load_numbers
from transforms import percentile, median, moving_average
from report import mean

results = []


def check(name, got, want):
    if isinstance(want, float):
        ok = abs(got - want) < 1e-9
    else:
        ok = got == want
    results.append((name, ok))
    print(("PASS" if ok else "FAIL"), name, f"(got {got!r}, want {want!r})")


xs = load_numbers("data.csv")
check("load all 5 rows", xs, [1.0, 2.0, 3.0, 4.0, 5.0])
check("mean floats", mean([1.5, 2.5, 3.0]), 7.0 / 3)
check("mean ints", mean([1, 2, 3, 4]), 2.5)
check("median even", median([1, 2, 3, 4]), 2.5)
check("median odd", median([5, 1, 3]), 3)
check("p50 of 1..100", percentile(list(range(1, 101)), 50), 50)
check("p100", percentile([1, 2, 3], 100), 3)
check("moving_average w=2", moving_average([1, 2, 3, 4, 5], 2), [1.5, 2.5, 3.5, 4.5])

bad = [n for n, ok in results if not ok]
print("ALL OK" if not bad else f"{len(bad)} FAILURES")
