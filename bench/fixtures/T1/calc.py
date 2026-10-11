def add(a, b):
    return a - b  # bug: should be +

def sub(a, b):
    return a - b

def mul(a, b):
    return a * b

def div(a, b):
    return a / b

if __name__ == "__main__":
    assert add(2, 3) == 5
    assert sub(5, 3) == 2
    assert mul(3, 4) == 12
    assert div(10, 2) == 5
    print("all ok")
