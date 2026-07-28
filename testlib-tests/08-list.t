use v5.36;

use Test::More tests => 3;

use TestLib::List;

is(
    TestLib::List::maybe_struct("foo", "bar", "baz"),
    "[foo] [bar] [baz]",
    "3-params to maybe_struct should work",
);

is(
    TestLib::List::maybe_struct({ first => "FOO", second => "BAR", third => "BAZ" }),
    "[FOO] [BAR] [BAZ]",
    "struct param to maybe_struct should work",
);

eval { TestLib::List::maybe_struct(qw(hey you)) };

is($@, "expected 1 or 3 parameters, got 2\n", "2 params to maybe_struct should fail");
