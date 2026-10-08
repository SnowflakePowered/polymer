------------------------- MODULE MCResourceLeases -------------------------
EXTENDS ResourceLeases, TLC

MCLaunches == {"L1", "L2"}
MCJobs == {"J1"}
MCKeys == {"a", "b", "h1", "h2"}
MCOverlap == {<<"h1", "h2">>}
MCJobKeys == {"a", "b"}
MCPlan ==
  ("L1" :> << [pre |-> <<>>, tail |-> TRUE, post |-> <<"a">>],
              [pre |-> <<"h1">>, tail |-> FALSE, post |-> <<>>] >>)
  @@
  ("L2" :> << [pre |-> <<"b">>, tail |-> TRUE, post |-> <<>>],
              [pre |-> <<"h2">>, tail |-> FALSE, post |-> <<>>] >>)
=============================================================================
