"""Small helper utilities."""


def banner(formal: bool) -> str:
    if formal:
        return "Good day"
    return "Hello"


def shout(text: str) -> str:
    return text.upper() + "!"
