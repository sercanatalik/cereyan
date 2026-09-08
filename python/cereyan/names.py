"""Generated two-word run names, unique within the store."""

from __future__ import annotations

import random

ADJECTIVES = [
    "amber", "brave", "calm", "clever", "cosmic", "crisp", "daring", "eager", "fluent",
    "gentle", "glossy", "golden", "humble", "jolly", "keen", "lively", "lucky", "merry",
    "mighty", "nimble", "noble", "patient", "polite", "proud", "quick", "quiet", "rapid",
    "rustic", "serene", "sharp", "silent", "sleek", "smooth", "steady", "sturdy", "sunny",
    "swift", "tidy", "vivid", "witty", "zesty",
]

NOUNS = [
    "badger", "beetle", "bison", "condor", "cougar", "coyote", "crane", "dolphin", "falcon",
    "ferret", "finch", "gecko", "heron", "ibis", "jackal", "koala", "lemur", "lynx", "macaw",
    "marmot", "meerkat", "narwhal", "newt", "ocelot", "osprey", "otter", "owl", "panda",
    "pelican", "puffin", "quail", "raven", "salmon", "sparrow", "stork", "tapir", "toucan",
    "walrus", "weasel", "wombat", "yak",
]


def candidate(rng: random.Random | None = None) -> str:
    rng = rng or random
    return f"{rng.choice(ADJECTIVES)}-{rng.choice(NOUNS)}"


def generate(store, rng: random.Random | None = None) -> str:
    """A two-word name that no run in the store has yet. After many collisions a
    numeric suffix guarantees termination."""
    for _ in range(64):
        name = candidate(rng)
        if not store.run_name_exists(name):
            return name
    base = candidate(rng)
    n = 2
    while store.run_name_exists(f"{base}-{n}"):
        n += 1
    return f"{base}-{n}"
