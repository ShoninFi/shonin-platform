#!/usr/bin/env python3
"""Regenerate bounded RPC helper code and exact vault/library identities after forge build."""
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EVM = ROOT / 'contracts/evm'
DEST = ROOT / 'packages/chain/src/sealed_views'


def keccak(code):
    return subprocess.check_output(['cast', 'keccak', '0x' + code.hex()], text=True).strip()


def main():
    identities = {}
    sources = {}
    for name in ['Vault', 'Upkeep', 'Claims', 'Posts', 'Valuations', 'ViewReads']:
        artifact = json.loads((EVM / 'out' / (name + '.sol') / (name + '.json')).read_text())
        deployed = artifact['deployedBytecode']
        masks = []
        for references in deployed.get('linkReferences', {}).values():
            for library, spans in references.items():
                assert library in ['Upkeep', 'Claims', 'Posts', 'Valuations']
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
        metadata = artifact['metadata']
        for path, details in metadata['sources'].items():
            current = keccak((EVM / path).read_bytes())
            assert current == details['keccak256'], f'stale build artifact: {path}'
            sources[path] = current
        if name == 'ViewReads':
            assert not masks
            (DEST / 'ViewReads.hex').write_text(code.hex() + '\n')
            (DEST / 'ViewReads.hash').write_text(keccak(code) + '\n')
        else:
            identities[name] = dict(length=len(code), normalized_hash=keccak(code), masks=masks)
    (DEST / 'identities.json').write_text(json.dumps(identities, indent=2) + '\n')
    (DEST / 'sources.json').write_text(json.dumps(sources, indent=2, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
