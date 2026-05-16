Output demo/pulci.gif

Set FontSize 14
Set Width 800
Set Height 280
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
