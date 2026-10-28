# Packages

Jig has no package registry: a module is a git repository, named by its URL and fetched at
a tag. This page is the index of modules people have published, and says how to use and
publish one. The rules are in [the reference](reference.md#files-and-packages).

## Using a module

Name it in your `jig.mod` with the tag you want, then fetch it:

```
module app
require github.com/someone/geom v1.2.0
```

```
$ jig get
```

`jig get` clones `https://github.com/someone/geom` at `v1.2.0` into the module cache and
records the commit in `jig.sum`, which you commit too. A file then imports one of the
module's packages by its full path, and uses it by the path's last segment:

```
import "github.com/someone/geom/shapes"

fn main() effects {io} {
  io.println(shapes.area(shapes.Square { side: 2.0 }).str())
}
```

`jig check`, `test` and `build` never fetch anything: what `jig.sum` names is what they
use, so a build does not change because a tag moved. When two modules require different
versions of a third, the highest is used. `replace github.com/someone/geom => ../geom`
points the path at a local directory while you work on both.

## Publishing a module

1. Put a `jig.mod` at the repository's root whose `module` line is the repository's path,
   such as `module github.com/you/geom`. Each directory of `.jig` files in it is a package.
2. Mark what other modules may use `pub`; everything else stays private to its package. A
   private function nothing uses is an error, so the public API is what keeps code alive.
3. Run what CI runs on it: `jig fmt --check`, `jig check`, `jig test` and `jig prove`.
   State contracts on the public functions; a caller's prover sees only those, never the
   bodies.
4. Tag a release, `git tag v0.1.0 && git push --tags`. A tag is a version; it should never
   move once published.
5. Add a row to the index below with a pull request.

## The index

| Module | Packages | What it does |
| --- | --- | --- |
| `std` (built in) | `std/io`, `fs`, `net`, `html`, `json`, `regex`, `hash`, `time`, `rand`, `os`, `task`, `math`, `lists`, `strings`, `sim` | The standard library, listed in [the reference](reference.md#standard-library) |

No module has been published yet. Add the first one: a row with its path, its packages and a
sentence on what it is for.
