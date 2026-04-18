# Burger To Cow

This project is about an interesting engineering challenge. Consider that we have a source plain text template file that is processed via the minijinja rust template engine, and have access to both the source file and the context map for injection, and the generated, expanded file.
Now, some other tool changes the expanded file, we want to reconstruct what a diff to the template would be, that means, telling a part what are changes to the template file vs variables .

Of course, this has no deterministic and generalizable solution. However given the specifics (being in possession of the source text, the value hash and able to instrument the template engine)  we can arrive at heuristics/ algos that , even if not for 100% of the cases , make the correct diff, leaving a minor percentage of occurrences as ambiguous.

Your task, construct test and verify, through a rust crate that you will create, such library, that is it can have a function that takes the same signature as minijinja's rendering one (so that it is transparent to users) and can do the minijinja transformation (and exerting control as necessary, i.e. keeping tabs of the generated output for later). Later on, passed the path to the original file, and an updated version of it, the library should create a standard diff with the diffs that should be merged to the template. In case there is no way to determine, the tool will generate a textual diff that says something like:
<<<< diff decision needed: start >>>>
# the original template contained: 
<lines of the original template>
# and the updated version has this, resolve this manually
<updated text
<<<< diff decision needed: end >>>>

You will create this as a pure rust lib, and an accompanying cli . Note that the core logic has to be 100% on the lib, so that lib should have the core api as pure rust, that gets regular rust data types, and recurs regular serializable results. 
the cli, done via clap as not to created unneeded work, is only about formatting the input and output and calling the Lib

This should include: 
- the core logic lib
- the testing cli
- a readme that explains the ideas , solution , assumptions and the trade-offs,  which include spelling out under what conditions we cannot reliably determine it.
- the unit test for this
- a set of text files as fixture for these tests and manual review

PS: I've put in initialramblings.md a list of possible ideas to explore. 
Use it as you find it usable, but there are in no way a recommendation nor a good approach, feel free to try your own path and or change these in small and large ways
