Output "/vhs/brainwire-demo.gif"
Output "/vhs/brainwire-demo.mp4"
Set Shell "bash"
Set FontSize 18
Set Width 1000
Set Height 600
Set Framerate 8
Set TypingSpeed 1ms
Type "brainwire --version"
Enter
Sleep 1s
Type `brainwire cypher --database /tmp/demo --ontology /vhs/ontology.json --query "CREATE (:Project {entity_id:'release', name:'Launch', priority:2})"`
Enter
Sleep 2s
Type `brainwire cypher --database /tmp/demo --query "MATCH (p:Project) RETURN p.name, p.priority"`
Enter
Sleep 3s
Type `brainwire cypher --database /tmp/demo --query "MATCH (p:Project) SET p.priority='high'"`
Enter
Sleep 4s
