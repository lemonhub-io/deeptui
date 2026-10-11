from dataclasses import dataclass, field

@dataclass
class User:
    name: str
    pw: str
    roles: list = field(default_factory=list)

@dataclass
class Post:
    title: str
    body: str
    author: str
