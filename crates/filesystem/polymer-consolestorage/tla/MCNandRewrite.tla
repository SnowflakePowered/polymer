---------------------------- MODULE MCNandRewrite ----------------------------
EXTENDS NandRewrite, TLC

CONSTANTS a, b, x, y, v1, v2

MCIds == {a, b}
MCFilePaths == {x, y}
MCVals == {v1, v2}
MCSymmetry == Permutations(MCIds) \cup Permutations(MCFilePaths) \cup Permutations(MCVals)
MCNoSilentLossNonTrailing == hist[Len(hist)].art = NoArt => NoSilentLoss
=============================================================================
