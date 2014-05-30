#![allow(dead_code)]

use url::Url;
use util::{CargoResult,ProcessBuilder,io_error,human_error,process};
use std::fmt;
use std::fmt::{Show,Formatter};
use std::str;
use std::io::{UserDir,AllPermissions};
use std::io::fs::{mkdir_recursive,rmdir_recursive,chmod};
use serialize::{Encodable,Encoder};
use core::source::Source;
use core::{NameVer,Package,Summary};
use ops;

#[deriving(Eq,Clone,Encodable)]
enum GitReference {
    Master,
    Other(String)
}

impl GitReference {
    pub fn for_str<S: Str>(string: S) -> GitReference {
        if string.as_slice() == "master" {
            Master
        } else {
            Other(string.as_slice().to_str())
        }
    }
}

impl Str for GitReference {
    fn as_slice<'a>(&'a self) -> &'a str {
        match *self {
            Master => "master",
            Other(ref string) => string.as_slice()
        }
    }
}

impl Show for GitReference {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

pub struct GitSource {
    remote: GitRemote,
    reference: GitReference,
    db_path: Path,
    checkout_path: Path,
    verbose: bool
}

impl GitSource {
    pub fn new(remote: GitRemote, reference: String, db: Path, checkout: Path, verbose: bool) -> GitSource {
        GitSource { remote: remote, reference: GitReference::for_str(reference), db_path: db, checkout_path: checkout, verbose: verbose }
    }
}

impl Show for GitSource {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        try!(write!(f, "git repo at {}", self.remote.url));

        match self.reference {
            Master => Ok(()),
            Other(ref reference) => write!(f, " ({})", reference)
        }
    }
}

impl Source for GitSource {
    fn update(&self) -> CargoResult<()> {
        let repo = try!(self.remote.checkout(&self.db_path));
        try!(repo.copy_to(self.reference.as_slice(), &self.checkout_path));

        Ok(())
    }

    fn list(&self) -> CargoResult<Vec<Summary>> {
        let pkg = try!(read_manifest(&self.checkout_path));
        Ok(vec!(pkg.get_summary().clone()))
    }

    fn download(&self, _: &[NameVer]) -> CargoResult<()> {
        Ok(())
    }

    fn get(&self, packages: &[NameVer]) -> CargoResult<Vec<Package>> {
        let pkg = try!(read_manifest(&self.checkout_path));

        if packages.iter().any(|nv| pkg.is_for_name_ver(nv)) {
            Ok(vec!(pkg))
        } else {
            Ok(vec!())
        }
    }
}

macro_rules! git(
    ($config:expr, $verbose:expr, $str:expr, $($rest:expr),*) => (
        try!(git_inherit(&$config, $verbose, format!($str, $($rest),*)))
    );

    ($config:expr, $verbose:expr, $str:expr) => (
        try!(git_inherit(&$config, $verbose, format!($str)))
    );
)

macro_rules! git_output(
    ($config:expr, $verbose:expr, $str:expr, $($rest:expr),*) => (
        try!(git_output(&$config, $verbose, format!($str, $($rest),*)))
    );

    ($config:expr, $verbose:expr, $str:expr) => (
        try!(git_output(&$config, $verbose, format!($str)))
    );
)

macro_rules! errln(
    ($($arg:tt)*) => (let _ = writeln!(::std::io::stdio::stderr(), $($arg)*))
)

/**
 * GitRemote represents a remote repository. It gets cloned into a local GitDatabase.
 */

#[deriving(Eq,Clone)]
pub struct GitRemote {
    url: Url,
    verbose: bool
}

#[deriving(Eq,Clone,Encodable)]
struct EncodableGitRemote {
    url: String
}

impl<E, S: Encoder<E>> Encodable<S, E> for GitRemote {
    fn encode(&self, s: &mut S) -> Result<(), E> {
        EncodableGitRemote {
            url: self.url.to_str()
        }.encode(s)
    }
}

/**
 * GitDatabase is a local clone of a remote repository's database. Multiple GitCheckouts
 * can be cloned from this GitDatabase.
 */

#[deriving(Eq,Clone)]
pub struct GitDatabase {
    remote: GitRemote,
    path: Path,
    verbose: bool
}

#[deriving(Encodable)]
pub struct EncodableGitDatabase {
    remote: GitRemote,
    path: String
}

impl<E, S: Encoder<E>> Encodable<S, E> for GitDatabase {
    fn encode(&self, s: &mut S) -> Result<(), E> {
        EncodableGitDatabase {
            remote: self.remote.clone(),
            path: self.path.display().to_str()
        }.encode(s)
    }
}

/**
 * GitCheckout is a local checkout of a particular revision. Calling `clone_into` with
 * a reference will resolve the reference into a revision, and return a CargoError
 * if no revision for that reference was found.
 */

pub struct GitCheckout {
    database: GitDatabase,
    location: Path,
    reference: GitReference,
    revision: String,
    verbose: bool
}

#[deriving(Encodable)]
pub struct EncodableGitCheckout {
    database: GitDatabase,
    location: String,
    reference: String,
    revision: String
}

impl<E, S: Encoder<E>> Encodable<S, E> for GitCheckout {
    fn encode(&self, s: &mut S) -> Result<(), E> {
        EncodableGitCheckout {
            database: self.database.clone(),
            location: self.location.display().to_str(),
            reference: self.reference.to_str(),
            revision: self.revision.to_str()
        }.encode(s)
    }
}

/**
 * Implementations
 */

impl GitRemote {
    pub fn new(url: Url, verbose: bool) -> GitRemote {
        GitRemote { url: url, verbose: verbose }
    }

    pub fn checkout(&self, into: &Path) -> CargoResult<GitDatabase> {
        if into.exists() {
            // TODO: If the revision we have is a rev, avoid unnecessarily fetching if we have the rev already
            try!(self.fetch_into(into));
        } else {
            try!(self.clone_into(into));
        }

        Ok(GitDatabase { remote: self.clone(), path: into.clone(), verbose: self.verbose })
    }

    fn fetch_into(&self, path: &Path) -> CargoResult<()> {
        Ok(git!(*path, self.verbose, "fetch --force --quiet --tags {} refs/heads/*:refs/heads/*", self.url))
    }

    fn clone_into(&self, path: &Path) -> CargoResult<()> {
        let dirname = Path::new(path.dirname());

        try!(mkdir_recursive(path, UserDir).map_err(|err|
            human_error(format!("Couldn't recursively create `{}`", dirname.display()), format!("path={}", dirname.display()), io_error(err))));

        Ok(git!(dirname, self.verbose, "clone {} {} --bare --no-hardlinks --quiet", self.url, path.display()))
    }
}

impl GitDatabase {
    fn get_path<'a>(&'a self) -> &'a Path {
        &self.path
    }

    pub fn copy_to<S: Str>(&self, reference: S, dest: &Path) -> CargoResult<GitCheckout> {
        let verbose = self.verbose;
        let checkout = try!(GitCheckout::clone_into(dest, self.clone(), GitReference::for_str(reference.as_slice()), verbose));

        try!(checkout.fetch());
        try!(checkout.update_submodules());

        Ok(checkout)
    }

    pub fn rev_for<S: Str>(&self, reference: S) -> CargoResult<String> {
        Ok(git_output!(self.path, self.verbose, "rev-parse {}", reference.as_slice()))
    }

}

impl GitCheckout {
    fn clone_into(into: &Path, database: GitDatabase, reference: GitReference, verbose: bool) -> CargoResult<GitCheckout> {
        let revision = try!(database.rev_for(reference.as_slice()));
        let checkout = GitCheckout { location: into.clone(), database: database, reference: reference, revision: revision, verbose: verbose };

        // If the git checkout already exists, we don't need to clone it again
        if !checkout.location.join(".git").exists() {
            try!(checkout.clone_repo());
        }

        Ok(checkout)
    }

    fn get_source<'a>(&'a self) -> &'a Path {
        self.database.get_path()
    }

    fn clone_repo(&self) -> CargoResult<()> {
        let dirname = Path::new(self.location.dirname());

        try!(mkdir_recursive(&dirname, UserDir).map_err(|e|
            human_error(format!("Couldn't mkdir {}", Path::new(self.location.dirname()).display()), None::<&str>, io_error(e))));

        if self.location.exists() {
            try!(rmdir_recursive(&self.location).map_err(|e|
                human_error(format!("Couldn't rmdir {}", Path::new(&self.location).display()), None::<&str>, io_error(e))));
        }

        git!(dirname, self.verbose, "clone --no-checkout --quiet {} {}", self.get_source().display(), self.location.display());
        try!(chmod(&self.location, AllPermissions).map_err(io_error));

        Ok(())
    }

    fn fetch(&self) -> CargoResult<()> {
        git!(self.location, self.verbose, "fetch --force --quiet --tags {}", self.get_source().display());
        try!(self.reset(self.revision.as_slice()));
        Ok(())
    }

    fn reset<T: Show>(&self, revision: T) -> CargoResult<()> {
        Ok(git!(self.location, self.verbose, "reset -q --hard {}", revision))
    }

    fn update_submodules(&self) -> CargoResult<()> {
        Ok(git!(self.location, self.verbose, "submodule update --init --recursive --quiet"))
    }
}

fn git(path: &Path, verbose: bool, str: &str) -> ProcessBuilder {
    if verbose {
        errln!("Executing git {} @ {}", str, path.display());
    }

    process("git").args(str.split(' ').collect::<Vec<&str>>().as_slice()).cwd(path.clone())
}

fn git_inherit(path: &Path, verbose: bool, str: String) -> CargoResult<()> {
    git(path, verbose, str.as_slice()).exec().map_err(|err|
        human_error(format!("Couldn't execute `git {}`: {}", str, err), None::<&str>, err))
}

fn git_output(path: &Path, verbose: bool, str: String) -> CargoResult<String> {
    let output = try!(git(path, verbose, str.as_slice()).exec_with_output().map_err(|err|
        human_error(format!("Couldn't execute `git {}`", str), None::<&str>, err)));

    Ok(to_str(output.output.as_slice()).as_slice().trim_right().to_str())
}

fn to_str(vec: &[u8]) -> String {
    str::from_utf8_lossy(vec).to_str()
}

fn read_manifest(path: &Path) -> CargoResult<Package> {
    let joined = path.join("Cargo.toml");
    ops::read_manifest(joined.as_str().unwrap())
}
