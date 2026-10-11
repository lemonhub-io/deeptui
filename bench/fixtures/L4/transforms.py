def moving_average(xs, w):
    """Averages of consecutive windows of size w (len(xs)-w+1 values)."""
    return [(xs[i] + xs[i + w]) / 2 for i in range(len(xs) - w)]


def percentile(xs, p):
    """Nearest-rank percentile: rank = ceil(p/100 * n), 1-based."""
    s = sorted(xs)
    return s[int(len(s) * p / 100)]


def median(xs):
    s = sorted(xs)
    n = len(s)
    if n % 2 == 1:
        return s[n // 2]
    return s[n // 2]
