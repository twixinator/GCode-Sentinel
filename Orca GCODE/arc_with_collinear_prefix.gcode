; Arc fitting + collinear merge fixture
; 4 collinear G1 moves along Y=5 (X axis), then 4 G1 moves approximating a CCW quarter circle
; No slicer header (unknown firmware) — should trigger W004 when arc-fit is used
G90
M82
G92 E0
; Collinear segment: 4 moves along X axis at Y=5, Z=0.2, F3000
; All on the same line (Y=5, Z=0.2), extrusion at 0.05 mm/mm
G0 X0.000000 Y5.000000 Z0.2 F9000
G1 X2.500000 Y5.000000 E0.125000 F3000
G1 X5.000000 Y5.000000 E0.250000
G1 X7.500000 Y5.000000 E0.375000
G1 X10.000000 Y5.000000 E0.500000
; CCW quarter circle: centre (10,15), r=10, 270deg to 360deg
; Start: (10, 5), intermediate steps at 22.5deg intervals, end: (20, 15)
G1 X13.826834 Y5.761205 E0.696350 F3000
G1 X17.071068 Y7.928932 E0.892699
G1 X19.238795 Y11.173166 E1.089049
G1 X20.000000 Y15.000000 E1.285398
M104 S0
