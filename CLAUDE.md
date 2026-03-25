this is a TUI for git graph. We want the user experience to be similar to the git graph plugin in VSCode, plus the git source control panel in vscode. 

use beads issue tracker - run `bd quickstart` for details. 

When implementing features, or fixes, follow this procedure:
* create a beads issue
* create a new git branch. 
* write unit tests for the feature where appropriate. 
* implement the feature, 
* ensure that tests pass, run linters and ensure they pass. 
* merge the branch. 
* close the beads issue. 
* document any learns learnt or gotchas or architectural decisions in the `docs` folder. 

of course ensure that you use software design principles and write code with the end goal of maintainability and extensibility in the long term. 
when fixing things, don't use bandaids for the symptoms, instead try to address the root cause
