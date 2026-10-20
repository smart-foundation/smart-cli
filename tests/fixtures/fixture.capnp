@0x9f3a7c21b4d8e5a6;
# A FIXTURE schema — deliberately OUTSIDE the canonical contract tree, and
# deliberately NOT compiled into the CLI library.
#
# It exists to prove one thing: the CLI can call a schema it has never
# compiled. The mock service serves this interface (it compiles the fixture
# for ITSELF, the way a real service compiles its own contract), and the CLI
# reaches it through the GENERIC path: the registry names the method, the
# service publishes the schema's node tree, and the CLI addresses the call
# from the contract's own introspection — with no generated binding on the
# client side at all.
#
# The CLI's build never puts these bindings into `crate::schema`: the mock's
# copy lives in the test support only.

struct EchoRequest {
    host @0 :Text;
    count @1 :UInt32 = 1;
}

struct EchoResponse {
    pong @0 :Text;
    count @1 :UInt32;
    ok @2 :Bool;
}

interface Echo {
    # The ordinals are SEQUENTIAL, so the method the CLI addresses generically
    # is NOT the first one: a client that addresses ordinal 0 by habit (or
    # by a hand-written table) calls the wrong method and the proof fails
    # loudly, not quietly.
    ping @0 () -> (response :EchoResponse);
    echo @1 (host :Text, count :UInt32 = 1) -> (response :EchoResponse);
}
