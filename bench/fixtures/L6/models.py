from dataclasses import dataclass


@dataclass
class Item:
    sku: str
    name: str
    price: float
