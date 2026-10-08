-------------------------- MODULE MCDownloadResume --------------------------
EXTENDS DownloadResume, TLC

MCVersions == {1, 2}
MCLengths == (1 :> 2) @@ (2 :> 2)
MCLengthsGrow == (1 :> 2) @@ (2 :> 3)
=============================================================================
