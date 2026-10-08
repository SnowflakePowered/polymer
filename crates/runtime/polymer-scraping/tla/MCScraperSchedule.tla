------------------------- MODULE MCScraperSchedule -------------------------
EXTENDS ScraperSchedule, TLC

All == {"error", "empty", "ok"}
Fails == {"error", "empty"}

ChainScrapers == {"a", "b", "c"}
ChainKeys == {"package", "title", "serial", "cover"}
ChainBase == {"package"}
ChainReq == ("a" :> {{"package"}}) @@ ("b" :> {{"title", "serial"}}) @@ ("c" :> {{"cover"}})
ChainOpt == ("a" :> {}) @@ ("b" :> {}) @@ ("c" :> {})
ChainProvides == ("a" :> {"title", "serial"}) @@ ("b" :> {"cover"}) @@ ("c" :> {})
ChainOutcomes == ("a" :> All) @@ ("b" :> All) @@ ("c" :> Fails)
ChainPrior == ("a" :> {}) @@ ("b" :> {}) @@ ("c" :> {})
ResumePrior == ("a" :> {"title"}) @@ ("b" :> {}) @@ ("c" :> {})

CycleScrapers == {"a", "b", "c", "d", "e"}
CycleKeys == {"package", "x", "y", "z", "w"}
CycleBase == {"package"}
CycleReq == ("a" :> {{"package"}}) @@ ("b" :> {{"package"}}) @@ ("c" :> {{"x"}, {"y"}})
            @@ ("d" :> {{"z"}}) @@ ("e" :> {{"w", "x"}})
CycleOpt == ("a" :> {{"y"}}) @@ ("b" :> {{"x"}}) @@ ("c" :> {}) @@ ("d" :> {}) @@ ("e" :> {})
CycleProvides == ("a" :> {"x"}) @@ ("b" :> {"y"}) @@ ("c" :> {}) @@ ("d" :> {"w"}) @@ ("e" :> {"z"})
CycleOutcomes == ("a" :> All) @@ ("b" :> All) @@ ("c" :> Fails) @@ ("d" :> All) @@ ("e" :> All)
CyclePrior == ("a" :> {}) @@ ("b" :> {}) @@ ("c" :> {}) @@ ("d" :> {}) @@ ("e" :> {})

WipeScrapers == {"h", "p"}
WipeKeys == {"package", "k", "t"}
WipeBase == {"package"}
WipeReq == ("h" :> {{"k"}}) @@ ("p" :> {{"package"}})
WipeOpt == ("h" :> {{"t"}}) @@ ("p" :> {})
WipeProvides == ("h" :> {}) @@ ("p" :> {"k", "t"})
WipeOutcomes == ("h" :> Fails) @@ ("p" :> All)
WipePrior == ("h" :> {}) @@ ("p" :> {"k"})
=============================================================================
