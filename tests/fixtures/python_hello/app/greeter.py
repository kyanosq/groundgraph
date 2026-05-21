"""Greeter implementation – used by the SpecSlice Python indexer fixture."""

from dataclasses import dataclass

from .utils import banner


@dataclass
class GreeterConfig:
    name: str
    formal: bool = False


class Greeter:
    """A toy class with a couple of methods so callHierarchy has something
    to chew on when a real Python LSP server is available."""

    def __init__(self, config: GreeterConfig) -> None:
        self.config = config

    def greet(self) -> str:
        prefix = banner(self.config.formal)
        return f"{prefix}, {self.config.name}!"

    def goodbye(self) -> str:
        return f"Bye, {self.config.name}!"


def make_greeter(name: str, *, formal: bool = False) -> Greeter:
    return Greeter(GreeterConfig(name=name, formal=formal))


async def make_greeter_async(name: str) -> Greeter:
    return make_greeter(name)
