"""Exercise the real installer in isolated directories, with no network or Rust build."""
import hashlib
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / 'install.sh'


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='deeptui migration ')
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.bin = self.root / 'bin'
        self.commands = self.root / 'commands'
        self.source = self.root / 'source'
        self.config = self.root / 'config'
        self.data = self.root / 'data'
        for path in [self.bin, self.commands, self.source / 'target/release']:
            path.mkdir(parents=True)
        (self.source / 'Cargo.toml').write_text('[package]\nname = "deeptui"\nversion = "0.1.0-beta"\n')
        self.executable(self.commands / 'cargo', '#!/bin/sh\nexit 0\n')
        self.executable(self.commands / 'rustc', '#!/bin/sh\nprintf "host: x86_64-unknown-linux-gnu\\n"\n')
        self.payload = self.source / 'target/release/deeptui'
        self.write_payload('0.1.0-beta')
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(('DEEPTUI_', 'RAGENT_'))}
        self.env.update(PATH=str(self.commands) + os.pathsep + os.environ['PATH'],
                        XDG_CONFIG_HOME=str(self.config), XDG_DATA_HOME=str(self.data),
                        NO_COLOR='1')

    @staticmethod
    def executable(path, contents):
        path.write_text(contents)
        path.chmod(0o755)

    def write_payload(self, version):
        self.executable(self.payload, '#!/bin/sh\n'
                        'if [ "$1" = --version ]; then\n'
                        f'  printf "deeptui {version}\\n"\n'
                        'elif [ "$1" = --test-model ]; then\n'
                        '  printf "%s\\n" "${DEEPTUI_MODEL:-unset}"\n'
                        'else\n  printf "%s\\n" "$@"\nfi\n')

    def install(self, *args, expected=0):
        result = subprocess.run(['bash', str(INSTALLER), '--source-dir', str(self.source),
                                 '--bin-dir', str(self.bin), *args], env=self.env,
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def release_fixture(self):
        assets = self.root / 'release'
        assets.mkdir()
        name = 'deeptui-v0.1.0-beta-x86_64-unknown-linux-musl.tar.gz'
        archive = assets / name
        with tarfile.open(archive, 'w:gz') as bundle:
            bundle.add(self.payload, arcname='deeptui-v0.1.0-beta-x86_64-unknown-linux-musl/deeptui')
        sums = assets / 'SHA256SUMS.txt'
        sums.write_text(f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {name}\n')
        self.env['DEEPTUI_TEST_ASSETS'] = str(assets)
        self.executable(self.commands / 'curl', '''#!/bin/sh
destination=""
url=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = -o ]; then shift; destination="$1"; else url="$1"; fi
  shift
done
asset="$DEEPTUI_TEST_ASSETS/${url##*/}"
[ -f "$asset" ] || exit 22
if [ -n "$destination" ]; then cp "$asset" "$destination"; fi
''')
        return sums

    def download(self, expected=0):
        result = subprocess.run(['bash', str(INSTALLER), '--bin-dir', str(self.bin),
                                 '--target', 'x86_64-unknown-linux-musl'], env=self.env,
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def legacy_files(self):
        old_config = self.config / 'ragent/config.toml'
        old_stats = self.data / 'ragent/stats.tsv'
        old_config.parent.mkdir(parents=True)
        old_stats.parent.mkdir(parents=True)
        old_config.write_text('api_key = "old-test-key"\nmodel = "example-model"\n')
        old_stats.write_text('1 example-model 100 10 0 0.1 ok\n')
        return old_config, old_stats

    def test_default_is_beta_and_old_command_forwards_to_new_binary(self):
        old_config, old_stats = self.legacy_files()
        self.executable(self.bin / 'ragent', '#!/bin/sh\necho ragent 0.6.0\n')
        self.install()
        config = self.config / 'deeptui/config.toml'
        stats = self.data / 'deeptui/stats.tsv'
        self.assertEqual(config.read_text(), old_config.read_text())
        self.assertEqual(stats.read_text(), old_stats.read_text())
        self.assertEqual(stat.S_IMODE(config.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(stats.stat().st_mode), 0o600)
        self.assertEqual(tomllib.loads(config.read_text())['api_key'], 'old-test-key')
        result = subprocess.check_output([str(self.bin / 'ragent'), '--version'], env=self.env, text=True)
        self.assertEqual(result.strip(), 'deeptui 0.1.0-beta')
        result = subprocess.check_output([str(self.bin / 'ragent'), 'a prompt with spaces'], env=self.env, text=True)
        self.assertEqual(result.strip(), 'a prompt with spaces')
        model_env = self.env | {'RAGENT_MODEL': 'legacy-model'}
        self.assertEqual(subprocess.check_output([str(self.bin / 'ragent'), '--test-model'], env=model_env, text=True).strip(), 'legacy-model')
        model_env['DEEPTUI_MODEL'] = 'new-model'
        self.assertEqual(subprocess.check_output([str(self.bin / 'ragent'), '--test-model'], env=model_env, text=True).strip(), 'new-model')

    def test_existing_settings_and_stats_are_preserved(self):
        self.legacy_files()
        for path, text in [(self.config / 'deeptui/config.toml', 'model = "new-model"\n'),
                           (self.data / 'deeptui/stats.tsv', 'new history\n')]:
            path.parent.mkdir(parents=True)
            path.write_text(text)
        self.install()
        self.assertEqual((self.config / 'deeptui/config.toml').read_text(), 'model = "new-model"\n')
        self.assertEqual((self.data / 'deeptui/stats.tsv').read_text(), 'new history\n')

    def test_retired_tags_cannot_replace_an_installed_beta(self):
        installed = self.bin / 'deeptui'
        self.executable(installed, '#!/bin/sh\necho existing-install\n')
        before = installed.read_bytes()
        for version in range(1, 7):
            with self.subTest(version=version):
                result = self.install('--tag', f'v0.{version}.0', expected=1)
                self.assertIn('unsupported', result.stderr)
                self.assertEqual(installed.read_bytes(), before)

    def test_latest_alias_and_unprefixed_beta_resolve_without_network(self):
        for tag in ['latest', '0.1.0-beta']:
            with self.subTest(tag=tag):
                self.install('--tag', tag)
                self.assertTrue((self.bin / 'deeptui').is_file())

    def test_stable_binary_cannot_masquerade_as_the_beta(self):
        self.executable(self.bin / 'deeptui', '#!/bin/sh\necho existing-install\n')
        self.write_payload('0.1.0')
        result = self.install(expected=1)
        self.assertIn('refusing to install', result.stderr)
        self.assertIn('existing-install', (self.bin / 'deeptui').read_text())
        self.assertFalse((self.config / 'deeptui/config.toml').exists())

    def test_unrelated_command_is_not_replaced(self):
        foreign = self.bin / 'ragent'
        self.executable(foreign, '#!/bin/sh\necho unrelated-tool\n')
        before = foreign.read_bytes()
        self.install()
        self.assertEqual(foreign.read_bytes(), before)

    def test_prebuilt_release_is_verified_and_installed(self):
        self.release_fixture()
        self.download()
        self.assertEqual(subprocess.check_output([str(self.bin / 'deeptui'), '--version'], text=True).strip(), 'deeptui 0.1.0-beta')

    def test_missing_checksum_cannot_replace_an_existing_install(self):
        self.release_fixture().unlink()
        self.executable(self.bin / 'deeptui', '#!/bin/sh\necho existing-install\n')
        result = self.download(expected=1)
        self.assertIn('refusing an unverified download', result.stderr)
        self.assertIn('existing-install', (self.bin / 'deeptui').read_text())

    def test_wrong_checksum_cannot_replace_an_existing_install(self):
        sums = self.release_fixture()
        sums.write_text('0' * 64 + '  deeptui-v0.1.0-beta-x86_64-unknown-linux-musl.tar.gz\n')
        self.executable(self.bin / 'deeptui', '#!/bin/sh\necho existing-install\n')
        result = self.download(expected=1)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertIn('existing-install', (self.bin / 'deeptui').read_text())

    def test_uninstall_removes_compatibility_launcher_and_keeps_config(self):
        self.legacy_files()
        self.executable(self.bin / 'ragent', '#!/bin/sh\necho ragent 0.5.0\n')
        self.install()
        self.install('--uninstall')
        self.assertFalse((self.bin / 'deeptui').exists())
        self.assertFalse((self.bin / 'ragent').exists())
        self.assertTrue((self.config / 'deeptui/config.toml').exists())


if __name__ == '__main__':
    unittest.main()
