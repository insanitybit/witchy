"""Tiny temporary-fixture controls for install-sdk-generation.py.

    /usr/bin/python3 -I -S -B scripts/test_sdk_generation_installer.py

Unprivileged only, and refuses to run as root: no real SDK, no sudo, nothing
under /Library/WitchySDK.  NOT exercised and NOT claimed: the positive
privileged path (store creation, root ownership, certificate minting,
protected-ancestor acceptance) and any real credential switch.  Staging is
exercised with the test user's own ids, which the tool never certifies.

Free space is faked as ample in every fixture, because the fixtures are a few
kilobytes while the tool's 16 GiB floor is real and this host may be below it;
the floor itself is tested with explicit faked values.  Tests named `mocked_*`
replace system calls to force a refusal or failure; they prove that logic only.
"""
import contextlib
import errno
import glob
import hashlib
import importlib.util
import io
import os
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

if os.geteuid() == 0:
    raise SystemExit('refusing to run fixture controls as root')
if not (sys.flags.isolated and sys.flags.no_site):
    raise SystemExit('run as: python3 -I -S -B ' + __file__)

SCRIPT = Path(__file__).with_name('install-sdk-generation.py')
spec = importlib.util.spec_from_file_location('sdk_installer', Path(__file__).with_name('install-sdk-generation.py'))
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)

IDS = (os.getuid(), os.getgid())
REAL_FREE_BYTES = tool.free_bytes
# The administrator bootstrap of the task result, with the digest left open.
BOOTSTRAP = '''import hashlib,sys
b=sys.stdin.buffer.read((1<<20)+1)
if len(b)>(1<<20) or hashlib.sha256(b).hexdigest()!="%s": sys.exit("installer bytes do not match the reviewed digest")
exec(compile(b,"install-sdk-generation","exec"),{"__name__":"__main__"})'''
DIR = os.O_RDONLY | os.O_DIRECTORY


def snapshot(root):
    """Path -> (lstat identity/mode/mtime, content or link target) for a whole tree."""
    seen = {}
    for base, dirs, files in os.walk(root):
        for name in dirs + files:
            path = os.path.join(base, name)
            st = os.lstat(path)
            if stat.S_ISLNK(st.st_mode):
                body = os.readlink(path)
            elif stat.S_ISREG(st.st_mode):
                body = Path(path).read_bytes()
            else:
                body = None
            seen[os.path.relpath(path, root)] = (st.st_ino, st.st_mode, st.st_nlink, st.st_uid, st.st_mtime_ns, body)
    return seen


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix='sdk-installer-test-'))
        self.addCleanup(self.remove_tmp)
        self.addCleanup(os.umask, os.umask(0o022))      # the tool's own umask must not shape fixtures
        self.sdk = os.path.join(self.tmp, 'sdk')
        os.makedirs(os.path.join(self.sdk, 'bin'))
        os.makedirs(os.path.join(self.sdk, 'lib', 'lean', 'Init'))
        self.write('bin/lean', b'#!fake lean\n', 0o755)
        self.write('lib/lean/Init.olean', b'olean-bytes' * 100, 0o644)
        self.write('lib/lean/Init/Core.olean', b'core', 0o664)
        os.link(self.path('lib/lean/Init/Core.olean'), self.path('lib/lean/Init/Alias.olean'))   # internal hardlink
        os.symlink('lean/Init.olean', self.path('lib/Init-link'))                                # internal relative
        os.symlink('../lib/lean', self.path('bin/libdir'))
        for name in ('staging', 'generations'):
            os.mkdir(os.path.join(self.tmp, name), 0o700)
        self.fds = []
        ample = mock.patch.object(tool, 'free_bytes', return_value=1 << 50)     # see module docstring
        ample.start()
        self.addCleanup(ample.stop)

    def remove_tmp(self):
        for fd in self.fds:
            os.close(fd)
        for base, dirs, _ in os.walk(self.tmp):
            for name in dirs:
                if not os.path.islink(os.path.join(base, name)):
                    os.chmod(os.path.join(base, name), 0o700)
        subprocess.run(['/bin/rm', '-rf', self.tmp], check=True)

    def path(self, relative):
        return os.path.join(self.sdk, relative)

    def write(self, relative, data, mode=0o644):
        Path(self.path(relative)).write_bytes(data)
        os.chmod(self.path(relative), mode)

    def open(self, path):
        self.fds.append(os.open(path, DIR))
        return self.fds[-1]

    def store(self):
        return self.open(os.path.join(self.tmp, 'staging')), self.open(os.path.join(self.tmp, 'generations'))

    def scan(self):
        return tool.scan_source(self.open(self.sdk), IDS[0])

    def staged(self):
        """Stage the fixture with the test user's ids; returns (source entries, sealed entries, dst path)."""
        source_fd = self.open(self.sdk)
        entries = tool.scan_source(source_fd, IDS[0])
        dst = os.path.join(self.tmp, 'staging', 'manual')
        os.mkdir(dst, 0o700)
        dst_fd = self.open(dst)
        tool.stage(source_fd, entries, dst_fd, IDS)
        return entries, tool.walk(dst_fd, IDS[0], IDS[1], sealed=True), dst

    def assert_store_empty(self):
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'staging')), [])
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'generations')), [])


class SourceValidation(Fixture):
    def test_valid_fixture_inventory(self):
        entries = self.scan()
        self.assertEqual([entry.path for entry in entries],
                         ['.', 'bin', 'bin/lean', 'bin/libdir', 'lib', 'lib/Init-link', 'lib/lean', 'lib/lean/Init',
                          'lib/lean/Init.olean', 'lib/lean/Init/Alias.olean', 'lib/lean/Init/Core.olean'])
        self.assertTrue(all(entry.payload == b'' for entry in entries if entry.kind != 'l'))   # nothing hashed

    def test_symlink_escapes_are_refused(self):
        outside = os.path.join(self.tmp, 'outside')
        Path(outside).write_bytes(b'secret')
        # '/lean/Init.olean' and '../../bin/lean' would name real SDK members if absolute or escaping parts were ignored.
        for target in (outside, '/lean/Init.olean', '../outside', '../../sdk/bin/lean', '../../bin/lean', 'missing',
                       'lean/Init.olean/x'):
            link = self.path('lib/bad')
            os.symlink(target, link)
            with self.assertRaises(tool.Refusal, msg=target):
                self.scan()
            os.unlink(link)
        self.scan()

    def test_cyclic_and_empty_links_are_refused(self):
        os.symlink('b', self.path('lib/a'))
        os.symlink('a', self.path('lib/b'))
        with self.assertRaisesRegex(tool.Refusal, 'cyclic'):
            self.scan()
        directory = tool.Entry('.', 'd', 1, 1, stat.S_IFDIR, 1, 0, 0, 0, b'')
        for target in (b'', b'/etc', b'a\nb'):
            with self.assertRaises(tool.Refusal):
                tool.check_links([directory, tool.Entry('l', 'l', 1, 2, stat.S_IFLNK, 1, 0, 0, 0, target)])

    def test_special_files_and_unsafe_names_are_refused(self):
        os.mkfifo(self.path('lib/pipe'))
        with self.assertRaisesRegex(tool.Refusal, 'not a regular file'):
            self.scan()                       # and it did not block opening the FIFO
        os.unlink(self.path('lib/pipe'))
        self.write('lib/new\nline', b'x')
        with self.assertRaisesRegex(tool.Refusal, 'unsafe name'):
            self.scan()

    def test_implausible_sdk_is_refused(self):
        os.chmod(self.path('bin/lean'), 0o644)
        with self.assertRaisesRegex(tool.Refusal, 'not a Lean SDK'):
            self.scan()
        os.unlink(self.path('bin/lean'))
        with self.assertRaisesRegex(tool.Refusal, 'not a Lean SDK'):
            self.scan()

    def test_wrong_owner_root_is_refused_before_listing(self):
        with mock.patch.object(tool.os, 'listdir', side_effect=AssertionError('listed')):   # spy, not behaviour
            with self.assertRaisesRegex(tool.Refusal, 'expected user'):
                tool.scan_source(self.open(self.sdk), IDS[0] + 1)

    def test_foreign_uid_hardlink_is_refused_before_open(self):
        """A real root-owned inode linked into the source: refused from lstat, never opened."""
        device = os.stat(self.tmp).st_dev
        foreign = None
        for candidate in sorted(glob.glob('/Library/Preferences/*.plist')) + ['/private/etc/hosts']:
            try:
                st = os.lstat(candidate)
                if stat.S_ISREG(st.st_mode) and st.st_uid != IDS[0] and st.st_dev == device:
                    os.link(candidate, self.path('lib/lean/Foreign.olean'))
                    foreign = candidate
                    break
            except OSError:
                continue
        if foreign is None:
            self.skipTest('host offers no foreign-owned file that an unprivileged user may hard-link here')
        opened = []
        real_open = os.open

        def spy(name, *args, **kwargs):
            opened.append(name)
            return real_open(name, *args, **kwargs)
        with mock.patch.object(tool.os, 'open', side_effect=spy):
            with self.assertRaisesRegex(tool.Refusal, 'Foreign.olean: not owned by the expected user'):
                tool.scan_source(self.open(self.sdk), IDS[0])
        self.assertNotIn('Foreign.olean', opened)


class Staging(Fixture):
    def test_layout_is_sealed_fresh_and_source_is_preserved(self):
        before = snapshot(self.sdk)
        entries, sealed, dst = self.staged()
        tool.compare(entries, sealed)
        self.assertEqual(snapshot(self.sdk), before)
        modes = {entry.path: stat.S_IMODE(entry.mode) for entry in sealed}
        self.assertEqual(modes['bin/lean'], 0o555)
        self.assertEqual(modes['lib/lean/Init.olean'], 0o444)
        self.assertEqual(modes['lib/lean/Init/Core.olean'], 0o444)      # group write dropped
        self.assertTrue(all(modes[entry.path] == 0o555 for entry in sealed if entry.kind in 'dl'))
        source_inodes = {(entry.dev, entry.ino) for entry in entries}
        for entry in sealed:
            self.assertNotIn((entry.dev, entry.ino), source_inodes)
            if entry.kind == 'f':
                self.assertEqual(entry.nlink, 1)                         # internal hardlinks become separate inodes
                self.assertEqual(entry.payload, hashlib.sha256(Path(self.path(entry.path)).read_bytes()).digest())
        self.assertEqual(os.readlink(os.path.join(dst, 'lib/Init-link')), 'lean/Init.olean')
        self.assertEqual(Path(dst, 'bin/libdir/Init.olean').read_bytes(), b'olean-bytes' * 100)

    def test_cow_clone_is_independent_of_later_source_writes(self):
        with mock.patch.object(tool, 'copy_file', side_effect=AssertionError('copy fallback used')):
            try:
                _, _, dst = self.staged()
            except OSError as error:
                self.skipTest('host temporary volume cannot clone: %s' % error)
        with open(self.path('lib/lean/Init.olean'), 'r+b') as handle:    # in-place write to the source inode
            handle.write(b'TAMPERED')
        os.chmod(self.path('bin/lean'), 0o777)
        self.assertEqual(Path(dst, 'lib/lean/Init.olean').read_bytes(), b'olean-bytes' * 100)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(dst, 'bin/lean')).st_mode), 0o555)

    def test_raw_fclonefileat_makes_a_new_inode(self):
        source = self.open(self.sdk)
        held = os.open('bin/lean', os.O_RDONLY, dir_fd=source)
        self.fds.append(held)
        try:
            tool.fclonefileat(held, self.open(self.tmp), 'clone')
        except OSError as error:
            self.skipTest('host temporary volume cannot clone: %s' % error)
        self.assertNotEqual(os.stat(os.path.join(self.tmp, 'clone')).st_ino, os.fstat(held).st_ino)
        with self.assertRaises(FileExistsError):
            tool.fclonefileat(held, self.open(self.tmp), 'clone')       # never overwrites

    def test_source_swapped_after_validation_is_refused(self):
        outside = os.path.join(self.tmp, 'outside')
        os.mkdir(outside)
        Path(outside, 'Core.olean').write_bytes(b'outside')

        def to_link():
            os.unlink(self.path('lib/lean/Init.olean'))
            os.symlink(os.path.join(outside, 'Core.olean'), self.path('lib/lean/Init.olean'))

        def to_other_file():
            os.unlink(self.path('lib/lean/Init.olean'))
            self.write('lib/lean/Init.olean', b'olean-bytes' * 100)

        def directory_to_link():
            os.rename(self.path('lib/lean/Init'), os.path.join(self.tmp, 'moved'))
            os.symlink(outside, self.path('lib/lean/Init'))

        for number, swap in enumerate((to_link, to_other_file, directory_to_link)):
            source_fd = self.open(self.sdk)
            entries = tool.scan_source(source_fd, IDS[0])
            swap()
            dst = os.path.join(self.tmp, 'staging', 'swap%d' % number)
            os.mkdir(dst, 0o700)
            with self.assertRaises(tool.Refusal, msg=swap.__name__):
                tool.stage(source_fd, entries, self.open(dst), IDS)
            self.assertFalse(any('outside' in str(body) for body in snapshot(dst).values()))
            if swap is to_link:
                os.unlink(self.path('lib/lean/Init.olean'))
                self.write('lib/lean/Init.olean', b'olean-bytes' * 100)

    def test_source_acl_is_refused_at_scan_and_reopen_and_sealed_acl_is_refused(self):
        def chmod(*arguments):
            subprocess.run(['/bin/chmod'] + list(arguments), check=True)
        # A failed assertion must not leave a deny-delete entry that blocks fixture removal.
        self.addCleanup(subprocess.run, ['/bin/chmod', '-h', '-N', self.path('lib/Init-link')])
        for relative in ('lib/lean/Init.olean', 'lib/lean/Init', 'lib', '.'):
            self.addCleanup(subprocess.run, ['/bin/chmod', '-N', self.path(relative)])
        deny = 'everyone deny delete'       # even an entry unrelated to reading is unsupported
        for relative in ('lib/lean/Init.olean', 'lib/lean/Init', 'lib', '.'):
            chmod('+a', deny, self.path(relative))
            with self.assertRaisesRegex(tool.Refusal, '%s: carries an ACL' % relative.replace('.', '[.]')):
                self.scan()
            chmod('-N', self.path(relative))
        link = self.path('lib/Init-link')
        chmod('-h', '+a', deny, link)        # on the symlink itself
        fd = os.open(link, tool.LINK_FLAGS)
        self.fds.append(fd)
        self.assertEqual(len(tool.acl_tags(fd)), 1)
        target = os.open(self.path('lib/lean/Init.olean'), os.O_RDONLY)
        self.fds.append(target)
        self.assertEqual(tool.acl_tags(target), [])
        with self.assertRaisesRegex(tool.Refusal, 'lib/Init-link: carries an ACL'):
            self.scan()
        chmod('-h', '-N', link)
        with mock.patch.object(tool, 'acl_tags', side_effect=OSError(errno.ENOTSUP, 'mocked')):     # fail closed
            with self.assertRaisesRegex(tool.Refusal, '[.]: ACL cannot be read'):
                self.scan()

        # An ACL added after validation is refused at the reopen, before that object is cloned.
        for number, relative in enumerate(('lib/lean/Init.olean', 'lib/lean/Init', 'lib/Init-link', '.')):
            source_fd = self.open(self.sdk)
            entries = tool.scan_source(source_fd, IDS[0])
            flags = ['-h'] if relative.endswith('link') else []
            chmod(*(flags + ['+a', deny, self.path(relative)]))
            dst = os.path.join(self.tmp, 'staging', 'acl%d' % number)
            os.mkdir(dst, 0o700)
            cloned, real = [], tool.fclonefileat
            with mock.patch.object(tool, 'fclonefileat', side_effect=lambda *a: (cloned.append(a[2]), real(*a))[1]):
                with self.assertRaisesRegex(tool.Refusal, 'carries an ACL', msg=relative):
                    tool.stage(source_fd, entries, self.open(dst), IDS)
            if relative == '.':
                self.assertEqual((cloned, os.listdir(dst)), ([], []))
            elif flags:
                self.assertFalse(os.path.lexists(os.path.join(dst, relative)))      # the link was not recreated
            else:
                self.assertNotIn({'lib/lean/Init.olean': 'Init.olean', 'lib/lean/Init': 'Core.olean'}[relative], cloned)
            chmod(*(flags + ['-N', self.path(relative)]))

        _, sealed, dst = self.staged()
        staged_file = os.path.join(dst, 'lib/lean/Init.olean')
        made = os.open(staged_file, os.O_RDONLY)
        self.fds.append(made)
        self.assertEqual(tool.acl_tags(made), [])
        chmod('+a', 'everyone allow write', staged_file)
        self.assertEqual(tool.acl_tags(made), [tool.ACL_EXTENDED_ALLOW])
        with self.assertRaisesRegex(tool.Refusal, 'carries an ACL'):
            tool.walk(self.open(dst), IDS[0], IDS[1], sealed=True)
        tool.strip_acl(made)
        self.assertEqual(tool.acl_tags(made), [])
        os.chmod(os.path.join(dst, 'lib'), 0o755)
        with self.assertRaisesRegex(tool.Refusal, 'not sealed'):
            tool.walk(self.open(dst), IDS[0], IDS[1], sealed=True)


class Publication(Fixture):
    def test_unprivileged_install_mints_nothing_and_cleans_its_staging(self):
        self.assertNotEqual(os.geteuid(), 0, 'these controls must not run as root')
        before = snapshot(self.sdk)
        source_fd = self.open(self.sdk)
        staging_fd, generations_fd = self.store()
        with self.assertRaisesRegex(tool.Refusal, 'no certificate'):
            tool.install_into(source_fd, tool.scan_source(source_fd, IDS[0]), staging_fd, generations_fd, IDS)
        self.assert_store_empty()
        self.assertEqual(snapshot(self.sdk), before)
        with self.assertRaisesRegex(tool.Refusal, 'no certificate'):      # claiming root ids does not help
            tool.mint_manifest(staging_fd, '0' * 64, [], (0, 0))
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'staging')), [])

    def test_command_dry_run_and_unprivileged_publication_create_nothing(self):
        existed = os.path.lexists(tool.STORE)
        before = snapshot(self.sdk)
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            self.assertEqual(tool.main(['--source', self.sdk, '--dry-run']), 0)
            self.assertEqual(tool.main(['--source', self.sdk]), 1)        # a path is never published
            self.addCleanup(os.chdir, os.getcwd())
            os.chdir(self.sdk)
            self.assertEqual(tool.main(['--source', '.']), 1)             # the only form, and it needs root
            self.assertEqual(tool.main(['--source', os.path.join(self.tmp, 'outside-link'), '--dry-run']), 1)
        self.assertIn('5 directories, 4 files, 2 symlinks', out.getvalue())
        self.assertIn('nothing was created', out.getvalue())
        self.assertNotIn('published', out.getvalue())
        self.assertEqual([line.split(':')[1].strip()[:28] for line in err.getvalue().splitlines()[:2]],
                         ['publication requires exactly', 'publication requires root (s'])
        self.assertEqual(os.path.lexists(tool.STORE), existed)
        self.assertEqual(snapshot(self.sdk), before)
        self.assert_store_empty()
        self.assertIn('dry-run: interpreter: ', out.getvalue())
        self.assertIn('bytes required under /Library', out.getvalue())
        with self.assertRaises(tool.Refusal):
            tool.open_store(self.open(self.tmp))
        for value in ('', '0', '-1', 'x', '１２', str(1 << 31)):
            with self.assertRaises(tool.Refusal, msg=value):
                tool.sudo_caller({'SUDO_UID': value, 'SUDO_GID': '20'})
            with self.assertRaises(tool.Refusal, msg=value):
                tool.sudo_caller({'SUDO_UID': '501', 'SUDO_GID': value})
        with self.assertRaises(tool.Refusal):
            tool.sudo_caller({'SUDO_UID': '501'})
        self.assertEqual(tool.sudo_caller({'SUDO_UID': '501', 'SUDO_GID': '20'}), tool.Caller(501, 20))

    def test_mocked_clone_failure_removes_only_this_runs_staging(self):
        before = snapshot(self.sdk)
        source_fd = self.open(self.sdk)
        staging_fd, generations_fd = self.store()
        os.mkdir(os.path.join(self.tmp, 'staging', 'unrelated'))
        real, calls = tool.fclonefileat, []

        def fail_second(*args):
            calls.append(args)
            if len(calls) == 2:
                raise OSError(errno.ENOSPC, 'mocked')
            return real(*args)
        with mock.patch.object(tool, 'fclonefileat', side_effect=fail_second):
            with self.assertRaises(OSError):
                tool.install_into(source_fd, tool.scan_source(source_fd, IDS[0]), staging_fd, generations_fd, IDS)
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'staging')), ['unrelated'])
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'generations')), [])
        self.assertEqual(snapshot(self.sdk), before)

    def test_mocked_clone_unsupported_copies_only_with_headroom(self):
        unsupported = mock.patch.object(tool, 'fclonefileat', side_effect=OSError(errno.EXDEV, 'mocked'))
        with unsupported, mock.patch.object(tool, 'free_bytes', return_value=tool.HEADROOM + 3):
            source_fd = self.open(self.sdk)
            entries = tool.scan_source(source_fd, IDS[0])
            dst = os.path.join(self.tmp, 'staging', 'short')
            os.mkdir(dst, 0o700)
            with self.assertRaisesRegex(tool.Refusal, 'insufficient free space'):
                tool.stage(source_fd, entries, self.open(dst), IDS)     # every fixture file is longer than 3 bytes
            self.assertFalse([name for name, seen in snapshot(dst).items() if stat.S_ISREG(seen[1])])
        with unsupported, mock.patch.object(tool, 'free_bytes', return_value=tool.HEADROOM + (1 << 20)):
            entries, sealed, dst = self.staged()
            tool.compare(entries, sealed)
            self.assertEqual(Path(dst, 'lib/lean/Init.olean').read_bytes(), b'olean-bytes' * 100)

    def test_cleanup_refuses_a_directory_it_did_not_create(self):
        staging_fd, _ = self.store()
        victim = os.path.join(self.tmp, 'staging', 'victim')
        os.mkdir(victim)
        Path(victim, 'keep').write_bytes(b'keep')
        with self.assertRaisesRegex(tool.Refusal, 'left untouched'):
            tool.remove_owned(staging_fd, 'victim', (0, 0))
        self.assertEqual(Path(victim, 'keep').read_bytes(), b'keep')
        tool.remove_owned(staging_fd, 'absent', (0, 0))
        st = os.stat(victim)
        tool.remove_owned(staging_fd, 'victim', (st.st_dev, st.st_ino))
        self.assertFalse(os.path.exists(victim))

    def test_publication_never_overwrites(self):
        staging_fd, generations_fd = self.store()
        for where in ('staging', 'generations'):
            os.mkdir(os.path.join(self.tmp, where, 'n'))
            Path(self.tmp, where, 'n', where).write_bytes(b'x')
        st = os.stat(os.path.join(self.tmp, 'staging', 'n'))
        with self.assertRaises(FileExistsError):
            tool.publish(staging_fd, generations_fd, 'n', (st.st_dev, st.st_ino))
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'staging', 'n')), ['staging'])
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'generations', 'n')), ['generations'])
        os.rename(os.path.join(self.tmp, 'staging', 'n'), os.path.join(self.tmp, 'staging', 'm'))
        tool.publish(staging_fd, generations_fd, 'm', (st.st_dev, st.st_ino))
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'generations', 'm')), ['staging'])

    def test_user_owned_paths_are_not_protected(self):
        with self.assertRaisesRegex(tool.Refusal, 'not root-owned'):
            tool.check_protected_path(self.path('bin/lean'))
        with self.assertRaisesRegex(tool.Refusal, 'not root-owned'):
            tool.require_protected(self.open(self.tmp), self.tmp, exact_mode=0o755)


class Corrections(Fixture):
    """Controls for review 299 (B1-B4, N1-N5)."""

    def test_source_must_be_other_readable_and_unflagged(self):
        for relative, private, public in (('lib/lean/Init.olean', 0o640, 0o644), ('lib/lean/Init', 0o750, 0o755),
                                          ('lib', 0o754, 0o755)):
            os.chmod(self.path(relative), private)
            with self.assertRaisesRegex(tool.Refusal, relative + ': not readable by others'):
                self.scan()
            os.chmod(self.path(relative), public)
        os.chflags(self.path('bin/lean'), stat.UF_HIDDEN)
        with self.assertRaisesRegex(tool.Refusal, 'bin/lean: unexpected BSD flags'):
            self.scan()
        os.chflags(self.path('bin/lean'), 0)
        self.scan()

    def test_flag_set_after_validation_is_refused_and_staging_is_removed(self):
        source_fd = self.open(self.sdk)
        entries = tool.scan_source(source_fd, IDS[0])
        staging_fd, generations_fd = self.store()
        os.chflags(self.path('lib/lean/Init.olean'), stat.UF_IMMUTABLE)
        self.addCleanup(os.chflags, self.path('lib/lean/Init.olean'), 0)
        with self.assertRaisesRegex(tool.Refusal, 'Init.olean: unexpected BSD flags appeared'):
            tool.install_into(source_fd, entries, staging_fd, generations_fd, IDS)
        self.assert_store_empty()       # the immutable clone did not survive as residue

    def test_mocked_space_floor_is_sixteen_gib_at_launch_and_throughout(self):
        self.assertEqual(tool.HEADROOM, 16 << 30)
        self.assertFalse(hasattr(tool, 'CLONE_FLOOR'))
        source_fd = self.open(self.sdk)
        entries = tool.scan_source(source_fd, IDS[0])
        total = sum(entry.size for entry in entries)
        self.assertEqual(total, sum(len(body) for *_, body in snapshot(self.sdk).values() if isinstance(body, bytes)))
        anywhere = self.open(self.tmp)
        self.assertFalse(hasattr(tool, 'require_launch_space'))     # no whole-SDK reservation at launch
        with mock.patch.object(tool, 'free_bytes', return_value=tool.HEADROOM - 1):
            with self.assertRaisesRegex(tool.Refusal, 'insufficient free space: %d bytes' % (16 << 30)):
                tool.require_space(anywhere)
        with mock.patch.object(tool, 'free_bytes', return_value=tool.HEADROOM):     # exactly the floor, however big the SDK
            tool.require_space(anywhere)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(tool.main(['--source', self.sdk, '--dry-run']), 0)
            self.assertIn('%d bytes required under /Library, %d available: enough' % (16 << 30, 16 << 30), out.getvalue())
        staging_fd, generations_fd = self.store()
        cloned = mock.patch.object(tool, 'fclonefileat', side_effect=AssertionError('cloned below the floor'))
        with cloned, mock.patch.object(tool, 'free_bytes', return_value=tool.HEADROOM - 1):
            with self.assertRaisesRegex(tool.Refusal, 'insufficient free space'):
                tool.install_into(source_fd, entries, staging_fd, generations_fd, IDS)
            dst = os.path.join(self.tmp, 'staging', 'low')
            os.mkdir(dst, 0o700)
            with self.assertRaisesRegex(tool.Refusal, 'insufficient free space'):
                tool.stage(source_fd, entries, self.open(dst), IDS)
            os.rmdir(dst)
        self.assert_store_empty()
        info = os.fstatvfs(anywhere)
        self.assertGreater(REAL_FREE_BYTES(anywhere), 0)
        self.assertLessEqual(abs(REAL_FREE_BYTES(anywhere) - info.f_bavail * info.f_frsize), 1 << 30)

    def test_credential_scope_is_inert_for_the_caller_and_refuses_without_root(self):
        before = (os.geteuid(), os.getegid(), tool.kernel_groups())
        self.assertEqual(before[2][0], os.getegid())
        with tool.source_access(None):
            self.assertEqual((os.geteuid(), os.getegid(), tool.kernel_groups()), before)
        with self.assertRaisesRegex(tool.Refusal, 'requires root'):
            with tool.source_access(tool.Caller(*IDS)):
                self.fail('entered a credential scope without root')
        self.assertEqual((os.geteuid(), os.getegid(), tool.kernel_groups()), before)

    def test_mocked_credential_switch_order_and_fail_closed(self):
        """Ordering and failure handling over a fake credential; the real switch is unqualified."""
        previous_mask = tool.signal.pthread_sigmask(tool.signal.SIG_UNBLOCK, tool.SIGNALS)
        self.addCleanup(tool.signal.pthread_sigmask, tool.signal.SIG_SETMASK, previous_mask)
        state = {'euid': 0, 'egid': 0, 'groups': [0, 1, 80]}
        calls, broken = [], set()

        def require_masked():
            self.assertTrue(set(tool.SIGNALS) <= tool.signal.pthread_sigmask(tool.signal.SIG_BLOCK, []))

        def setgroups(groups):
            require_masked()
            calls.append(('groups', list(groups)))
            if state['euid'] != 0 or 'groups' in broken:
                raise PermissionError(errno.EPERM, 'mocked')
            state['groups'], state['egid'] = list(groups), groups[0]

        def setegid(gid):
            require_masked()
            calls.append(('egid', gid))
            state['egid'] = gid

        def seteuid(uid):
            require_masked()
            calls.append(('euid', uid))
            if ('euid', uid) in broken:
                raise PermissionError(errno.EPERM, 'mocked')
            state['euid'] = uid

        fake = contextlib.ExitStack()
        self.addCleanup(fake.close)
        for name, function in (('setgroups', setgroups), ('setegid', setegid), ('seteuid', seteuid),
                               ('geteuid', lambda: state['euid']), ('getegid', lambda: state['egid']),
                               ('_exit', mock.Mock(side_effect=SystemExit('aborted')))):
            fake.enter_context(mock.patch.object(tool.os, name, function))
        fake.enter_context(mock.patch.object(tool, 'kernel_groups', lambda: list(state['groups'])))
        fake.enter_context(mock.patch.object(tool.os, 'write', lambda fd, data: len(data)))

        with tool.source_access(tool.Caller(501, 20)):
            self.assertEqual(state, {'euid': 501, 'egid': 20, 'groups': [20]})
        self.assertEqual(calls, [('groups', [20]), ('egid', 20), ('euid', 501),
                                 ('euid', 0), ('groups', [0]), ('egid', 0)])
        self.assertEqual(state, {'euid': 0, 'egid': 0, 'groups': [0]})

        with self.assertRaises(KeyError):                   # an error inside the scope still restores
            with tool.source_access(tool.Caller(501, 20)):
                raise KeyError('inside')
        self.assertEqual(state, {'euid': 0, 'egid': 0, 'groups': [0]})

        broken.add(('euid', 501))                           # incomplete switch: refuse, restored
        with self.assertRaisesRegex(tool.Refusal, 'could not assume'):
            with tool.source_access(tool.Caller(501, 20)):
                self.fail('entered with an incomplete switch')
        self.assertEqual(state, {'euid': 0, 'egid': 0, 'groups': [0]})

        broken.clear()
        with self.assertRaisesRegex(SystemExit, 'aborted'):  # incomplete restoration: abort the process
            with tool.source_access(tool.Caller(501, 20)):
                broken.add(('euid', 0))
        tool.os._exit.assert_called_once_with(70)
        fake.close()

    def test_stage_touches_the_source_only_inside_the_caller_scope(self):
        """Spy stand-in for the credential scope: source opens inside it, clones outside it."""
        previous_mask = tool.signal.pthread_sigmask(tool.signal.SIG_UNBLOCK, tool.SIGNALS)
        self.addCleanup(tool.signal.pthread_sigmask, tool.signal.SIG_SETMASK, previous_mask)
        caller, depth, seen = tool.Caller(501, 20), [0], []

        @contextlib.contextmanager
        def scope(given):
            self.assertIs(given, caller)
            self.assertTrue(set(tool.SIGNALS) <= tool.signal.pthread_sigmask(tool.signal.SIG_BLOCK, []))
            depth[0] += 1
            try:
                yield
            finally:
                self.assertTrue(set(tool.SIGNALS) <= tool.signal.pthread_sigmask(tool.signal.SIG_BLOCK, []))
                depth[0] -= 1

        def inside(real, label):
            def spy(*args, **kwargs):
                self.assertEqual(depth[0], 1, label)
                seen.append(label)
                return real(*args, **kwargs)
            return spy

        def outside(real, label):
            def spy(*args, **kwargs):
                self.assertEqual(depth[0], 0, label)
                self.assertFalse(set(tool.SIGNALS) & tool.signal.pthread_sigmask(tool.signal.SIG_BLOCK, []))
                seen.append(label)
                return real(*args, **kwargs)
            return spy

        source_fd = self.open(self.sdk)
        entries = tool.scan_source(source_fd, IDS[0])
        dst = os.path.join(self.tmp, 'staging', 'scoped')
        os.mkdir(dst, 0o700)
        with mock.patch.object(tool, 'source_access', scope), \
                mock.patch.object(tool, 'open_same', inside(tool.open_same, 'open')), \
                mock.patch.object(tool.os, 'readlink', inside(os.readlink, 'readlink')), \
                mock.patch.object(tool, 'fclonefileat', outside(tool.fclonefileat, 'clone')), \
                mock.patch.object(tool, 'seal', outside(tool.seal, 'seal')):
            tool.stage(source_fd, entries, self.open(dst), IDS, caller)
        kinds = [entry.kind for entry in entries[1:]]
        self.assertEqual(seen.count('open'), len(kinds))        # every directory, file and symlink reopen
        self.assertEqual(seen.count('readlink'), kinds.count('l'))
        self.assertEqual(seen.count('clone'), kinds.count('f'))

    def test_mocked_durability_failure_after_rename_is_reported_as_published(self):
        staging_fd, generations_fd = self.store()
        os.mkdir(os.path.join(self.tmp, 'staging', 'n'))
        st = os.stat(os.path.join(self.tmp, 'staging', 'n'))
        with mock.patch.object(tool, 'full_sync', side_effect=OSError(errno.EIO, 'mocked')):
            with self.assertRaises(tool.PublishedUnconfirmed) as caught:
                tool.publish(staging_fd, generations_fd, 'n', (st.st_dev, st.st_ino))
        self.assertEqual(caught.exception.nonce, 'n')
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'generations')), ['n'])
        self.assertEqual(os.listdir(os.path.join(self.tmp, 'staging')), [])
        err = io.StringIO()
        with mock.patch.object(tool, 'run', side_effect=tool.PublishedUnconfirmed('ab' * 32, 'mocked')):
            with contextlib.redirect_stderr(err):
                self.assertEqual(tool.main([]), 2)
        self.assertIn('PUBLISHED, but completion reporting or durability is unconfirmed', err.getvalue())
        self.assertIn('nonce: ' + 'ab' * 32, err.getvalue())
        self.assertNotIn('refused', err.getvalue())

    def test_mocked_acl_failure_cannot_reuse_a_previous_no_acl_errno(self):
        """A NULL result without errno is an unknown failure, never an empty ACL."""
        tool.ctypes.set_errno(errno.ENOENT)
        with mock.patch.object(tool._libc, 'acl_get_fd_np', return_value=None):
            with self.assertRaises(OSError):
                tool.acl_tags(-1)

    def test_published_report_failure_keeps_the_nonce_and_exit_code(self):
        """Output handling only; no installation, credential switch or certificate."""
        class BrokenFlush(io.StringIO):
            def flush(self):
                raise BrokenPipeError(errno.EPIPE, 'mocked closed output pipe')

        nonce = 'ab' * 32
        err = io.StringIO()
        with mock.patch.object(tool, 'run', side_effect=lambda _: tool.report_publication(nonce, 'report fixture')):
            with contextlib.redirect_stdout(BrokenFlush()), contextlib.redirect_stderr(err):
                self.assertEqual(tool.main([]), 2)
        self.assertIn('PUBLISHED', err.getvalue())
        self.assertIn('nonce: ' + nonce, err.getvalue())
        with mock.patch.object(tool, 'run', side_effect=tool.PublishedUnconfirmed(nonce, 'mocked')):
            with contextlib.redirect_stderr(BrokenFlush()):
                self.assertEqual(tool.main([]), 2)

        # Real closed pipes must keep exit 2 through interpreter finalization too.
        code = '''import sys
scope = {"__name__": "report_fixture"}
with open(sys.argv[1], "rb") as source:
    exec(compile(source.read(), "report_fixture", "exec"), scope)
scope["run"] = lambda _: scope["report_publication"](sys.argv[2], "report fixture")
raise SystemExit(scope["main"]([]))
'''
        for closed_stderr in (False, True):
            reader, writer = os.pipe()
            os.close(reader)
            try:
                result = subprocess.run([sys.executable, '-I', '-S', '-B', '-c', code, str(SCRIPT), nonce],
                                        stdout=writer, stderr=writer if closed_stderr else subprocess.PIPE)
            finally:
                os.close(writer)
            self.assertEqual(result.returncode, 2)
            if not closed_stderr:
                self.assertIn(nonce.encode(), result.stderr)

    def test_digest_pinned_bootstrap_shebang_and_startup_flags(self):
        """The result's administrator literal, run unprivileged with --dry-run only."""
        system = '/usr/bin/python3'
        if not os.path.exists(system):
            self.skipTest('no system interpreter')
        data = SCRIPT.read_bytes()

        def launch(arguments, feed=None):
            return subprocess.run(arguments + ['--source', '.'], cwd=self.sdk, input=feed, capture_output=True)

        pinned = [system, '-I', '-S', '-B', '-c', BOOTSTRAP % hashlib.sha256(data).hexdigest()]
        good = launch(pinned + ['--dry-run'], data)
        self.assertEqual(good.returncode, 0, good.stderr)
        self.assertIn(b'dry-run: source is acceptable: 5 directories, 4 files, 2 symlinks', good.stdout)
        self.assertIn(b'dry-run: interpreter: acceptable', good.stdout)
        publish = launch(pinned, data)
        self.assertEqual((publish.returncode, publish.stdout), (1, b''))
        self.assertIn(b'requires root', publish.stderr)
        by_path = subprocess.run(pinned + ['--source', self.sdk], input=data, capture_output=True)
        self.assertEqual((by_path.returncode, by_path.stdout), (1, b''))
        self.assertIn(b'requires exactly --source .', by_path.stderr)
        for altered in (data + b'\n', data[:-1], b''):
            bad = launch(pinned + ['--dry-run'], altered)
            self.assertNotEqual(bad.returncode, 0)
            self.assertEqual(bad.stdout, b'')
            self.assertIn(b'do not match the reviewed digest', bad.stderr)
        huge = launch(pinned + ['--dry-run'], data + b'#' * (1 << 20))
        self.assertIn(b'do not match the reviewed digest', huge.stderr)

        self.assertEqual(data.splitlines()[0], b'#!/usr/bin/python3 -ISB')     # one argument: no splitting relied on
        self.assertEqual(launch([str(SCRIPT), '--dry-run']).returncode, 0)
        for flags in ([], ['-I'], ['-S']):
            loose = launch([system] + flags + ['-B', str(SCRIPT), '--dry-run'])
            self.assertEqual((loose.returncode, loose.stdout), (1, b''), flags)
            self.assertIn(b'start the interpreter with -I -S', loose.stderr)
        self.assertNotIn(b'__file__', data)
        self.assertEqual(os.path.lexists(tool.STORE), False)


class ManifestFraming(Fixture):
    def encoded(self):
        _, sealed, _ = self.staged()
        return sealed, tool.encode_manifest('ab' * 32, '/Library/WitchySDK/generations/%s/sdk' % ('ab' * 32),
                                            'arm64', (7, 9), sealed)

    def test_frames_are_u64_le_length_prefixed_and_round_trip(self):
        sealed, data = self.encoded()
        frames, rest = [], data
        while rest:                                   # independent reading of the existing Rust unit_frame format
            (length,) = struct.unpack('<Q', rest[:8])
            frames.append(rest[8:8 + length])
            self.assertEqual(len(frames[-1]), length)
            rest = rest[8 + length:]
        self.assertEqual(frames[:7], [b'witchy-sdk-generation-manifest-1', b'install-sdk-generation policy 2',
                                      b'darwin-arm64', b'ab' * 32, b'origin-and-sealed-bytes-only',
                                      b'content-policy=none', b'xattrs=cloned-unhashed-unverified'])
        self.assertEqual(frames[7], b'/Library/WitchySDK/generations/' + b'ab' * 32 + b'/sdk')
        self.assertEqual(struct.unpack('<QQ', frames[8]), (7, 9))
        self.assertEqual(struct.unpack('<Q', frames[9]), (len(sealed),))
        self.assertEqual(len(frames), 10 + 3 * len(sealed) + 1)
        self.assertEqual(frames[-1], hashlib.sha256(data[:-40]).digest())
        decoded = tool.decode_manifest(data)
        self.assertEqual(decoded['entries'], sealed)
        self.assertEqual((decoded['nonce'], decoded['generation'], decoded['platform']), ('ab' * 32, (7, 9), 'darwin-arm64'))

    def test_a_stronger_role_or_policy_claim_is_not_this_protocol(self):
        sealed, _ = self.encoded()
        for name, claimed in (('ROLE', b'lean-sdk-root'), ('CONTENT_POLICY', b'content-policy=reviewed'),
                              ('XATTR_POLICY', b'xattrs=verified'), ('ISSUER', b'install-sdk-generation policy 1')):
            with mock.patch.object(tool, name, claimed):
                forged = tool.encode_manifest('ab' * 32, '/x', 'arm64', (7, 9), sealed)
            with self.assertRaises(ValueError, msg=name):
                tool.decode_manifest(forged)

    def test_damaged_manifests_are_rejected(self):
        _, data = self.encoded()
        flipped = bytearray(data)
        flipped[len(data) // 2] ^= 1
        for bad in (data[:-1], data + b'\0', bytes(flipped), b'', data[:40], struct.pack('<Q', 1 << 62) + data):
            with self.assertRaises(ValueError):
                tool.decode_manifest(bad)


if __name__ == '__main__':
    unittest.main()
