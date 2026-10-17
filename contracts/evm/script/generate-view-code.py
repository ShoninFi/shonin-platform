#!/usr/bin/env python3
"""After `forge build`, write the chain follower's code files.

- packages/chain/src/follow/code: the ViewReads runtime code and its hash, the vault
  code identities (length, hash with link and immutable spans zeroed, and the spans) and
  the hashes of every source they were built from.
- packages/chain/tests/stand_in_node/code.json: the runtime code of the contracts the
  integration tests put on a local node.

Runs from any directory; every path is taken relative to the repository root.
"""
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EVM = ROOT / 'contracts/evm'
DEST = ROOT / 'packages/chain/src/follow/code'
STAND_IN = ROOT / 'packages/chain/tests/stand_in_node'

LIBRARIES = ['Upkeep', 'Claims', 'Posts', 'Valuations']
STAND_INS = ['FollowProbe', 'StandInMulticall3', 'MockArbSys']


def keccak(code):
    return subprocess.check_output(['cast', 'keccak', '0x' + code.hex()], text=True).strip()


def artifact(name):
    return json.loads((EVM / 'out' / (name + '.sol') / (name + '.json')).read_text())


def check_sources(found, sources):
    """Refuses an artifact built from a source that has changed since."""
    for path, details in found['metadata']['sources'].items():
        current = keccak((EVM / path).read_bytes())
        assert current == details['keccak256'], f'stale build artifact: {path}'
        if sources is not None:
            sources[path] = current


def main():
    DEST.mkdir(parents=True, exist_ok=True)
    identities = {}
    sources = {}
    for name in ['Vault', *LIBRARIES, 'ViewReads']:
        found = artifact(name)
        deployed = found['deployedBytecode']
        masks = []
        for references in deployed.get('linkReferences', {}).values():
            for library, spans in references.items():
                assert library in LIBRARIES
                for span in spans:
                    assert span['length'] == 20
                    masks.append(dict(role=library, **span))
        for field, spans in deployed.get('immutableReferences', {}).items():
            assert field == 'library_deploy_address' and name not in ['Vault', 'ViewReads']
            for span in spans:
                assert span['length'] == 32
                masks.append(dict(role='self', **span))
        chars = list(deployed['object'].removeprefix('0x'))
        for mask in masks:
            start, size = mask['start'] * 2, mask['length'] * 2
            chars[start:start + size] = ['0'] * size
        code = bytes.fromhex(''.join(chars))
        check_sources(found, sources)
        if name == 'ViewReads':
            assert not masks
            (DEST / 'ViewReads.hex').write_text(code.hex() + '\n')
            (DEST / 'ViewReads.hash').write_text(keccak(code) + '\n')
        else:
            identities[name] = dict(length=len(code), normalized_hash=keccak(code), masks=masks)
    (DEST / 'identities.json').write_text(json.dumps(identities, indent=2) + '\n')
    (DEST / 'sources.json').write_text(json.dumps(sources, indent=2, sort_keys=True) + '\n')

    stand_ins = {}
    for name in STAND_INS:
        found = artifact(name)
        deployed = found['deployedBytecode']
        assert not deployed.get('linkReferences'), f'{name} links a library'
        assert not deployed.get('immutableReferences'), f'{name} has immutables'
        check_sources(found, None)
        stand_ins[name] = deployed['object']
    STAND_IN.mkdir(parents=True, exist_ok=True)
    (STAND_IN / 'code.json').write_text(json.dumps(stand_ins, indent=2, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
