Output demo/pulci.gif

Set FontSize 18
Set Width 1200
Set Height 480
Set Padding 24
Set BorderRadius 0
Set TypingSpeed 75ms

Set Theme '{"name":"PulciBlack","background":"#000000","foreground":"#e0e0e0","cursor":"#ffffff","selection":"#333333","black":"#000000","red":"#f25757","green":"#7cd991","yellow":"#f0c75e","blue":"#6ba4ed","magenta":"#c98ff0","cyan":"#5fc7d1","white":"#e0e0e0","brightBlack":"#666666","brightRed":"#ff8585","brightGreen":"#a3eab5","brightYellow":"#f9d987","brightBlue":"#8fbef0","brightMagenta":"#dba6f5","brightCyan":"#85d6df","brightWhite":"#ffffff"}'

Hide
Type "alias pulci='bash demo/_sim.sh'"
Enter
Sleep 200ms
Ctrl+L
Show

Type "pulci start ."
Enter
Sleep 2500ms

Type "pulci status"
Enter
Sleep 3000ms

Type "# edited src/api.py — removed the unused import"
Enter
Sleep 1000ms

Type "pulci status"
Enter
Sleep 3500ms
