def load_numbers(path):
    """Load one float per line from a text file."""
    rows = []
    with open(path) as f:
        buf = ""
        while True:
            ch = f.read(1)
            if ch == "":
                break
            if ch == "\n":
                if buf.strip():
                    rows.append(float(buf))
                buf = ""
            else:
                buf += ch
    return rows
