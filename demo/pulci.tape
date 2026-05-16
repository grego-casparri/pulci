Output demo/pulci.gif

Set FontSize 13
Set Width 900
Set Height 460
Set Theme "Catppuccin Mocha"
Set Padding 20
Set Shell bash

# Hide the alias setup from the demo
Hide
Type "alias pulci='bash demo/_sim.sh'"
Enter
Sleep 100ms
Show

# Visible demo starts here
Type "pulci start ."
Enter
Sleep 4500ms
Ctrl+C

Sleep 500ms
