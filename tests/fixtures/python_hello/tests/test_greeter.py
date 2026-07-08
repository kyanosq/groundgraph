"""pytest cases used by the GroundGraph Python AST fixture."""

import pytest

from app.greeter import Greeter, make_greeter


@pytest.fixture
def casual_greeter() -> Greeter:
    return make_greeter("Ada")


def test_greet_casual(casual_greeter: Greeter) -> None:
    assert casual_greeter.greet() == "Hello, Ada!"


@pytest.mark.parametrize("name", ["Linus", "Grace"])
def test_make_greeter_supports_names(name: str) -> None:
    greeter = make_greeter(name)
    assert greeter.greet().endswith(f"{name}!")


class TestGoodbye:
    def test_uses_name(self) -> None:
        greeter = make_greeter("Ada")
        assert greeter.goodbye() == "Bye, Ada!"
