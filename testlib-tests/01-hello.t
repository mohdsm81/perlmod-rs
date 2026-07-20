use v5.36;

use Test::More tests => 16;

use TestLib::Hello;

my $greeting = eval { TestLib::Hello::hello("Testsuite") } // $@;
is($greeting, "Hello, 'Testsuite'", 'test library loads correctly');

my ($x, $y) = TestLib::Hello::multi_return();
is($x, 17, "first multi_return value should be 17");
is($y, 32, "second multi_return value should be 32");

my $param = { a => 1 };
is(
    TestLib::Hello::opt_string($param->{x}),
    "Called with None.",
    "non-existent element passed to Option<String>",
);
ok(!exists($param->{x}), "param->{x} was not auto-vivified");
is(
    TestLib::Hello::opt_str($param->{x}),
    "Called with None.",
    "non-existent element passed to Option<&str>",
);
ok(!exists($param->{x}), "param->{x} was not auto-vivified (2)");

is(
    TestLib::Hello::trailing_optional(1, 99),
    '1, Some(99)',
    'passing value for trailing optional parameter',
);
is(
    TestLib::Hello::trailing_optional(2, undef),
    '2, None',
    'passing undef for trailing optional parameter',
);
is(TestLib::Hello::trailing_optional(3), '3, None', 'skipping trailing optional parameter');

is(
    TestLib::Hello::trailing_list(20, 21, 22),
    'first=20, rest has 2 parameters',
    'collecting >0 trailing list parameters',
);
is(
    TestLib::Hello::trailing_list(25),
    'first=25, rest has 0 parameters',
    'collecting 0 trailing list parameters',
);
is(
    TestLib::Hello::trailing_list_and_options(30, 31, 32, 33, 34),
    '1st=30, 2nd=Some(31), rest has 3 parameters',
    'final option, then list',
);
is(
    TestLib::Hello::trailing_list_and_options(35, 36),
    '1st=35, 2nd=Some(36), rest has 0 parameters',
    'final option, empty list',
);
is(
    TestLib::Hello::trailing_list_and_options(37),
    '1st=37, 2nd=None, rest has 0 parameters',
    'final option unset, empty list',
);
is(TestLib::Hello::sum_list(50, 51, 52), 153, 'pass a deserialized list');
