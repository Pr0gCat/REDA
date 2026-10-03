"""Check the independent auditor against a real record and corrupted copies."""
import copy
import sys

from audit_fragment_search_wave import audit, compare, read


def main(path):
    records = read(path)
    assert audit(records)['hidden_extra_transitions'] == 0
    assert compare(records, records)['regressions'] == []

    def corrupt_snapshot(data):
        label = next(iter(data[1]['signals']))
        data[1]['signals'][label] = not data[1]['signals'][label]

    def corrupt_oracle(data):
        data[0]['gates'][0]['kind'] = 'Unsupported(1)'

    def drop_phase(data):
        data.pop(1)

    def reset_history(data):
        data[2]['start'] = data[1]['start'] + data[1]['ticks'] + 1

    def invent_event(data):
        label = next(iter(data[0]['definitions']))
        data[1]['events'].append({'label': label, 'tick': data[1]['start'],
                                  'value': data[0]['power_on'][label]})

    def corrupt_world_binding(data):
        data[0]['world_fingerprint'] = 'wrong-world'

    for change in [corrupt_snapshot, corrupt_oracle, drop_phase, reset_history,
                   invent_event, corrupt_world_binding]:
        changed = copy.deepcopy(records)
        change(changed)
        try:
            audit(changed)
        except AssertionError:
            continue
        raise AssertionError(f'{change.__name__} was admitted')
    print('one real record and six corrupted records checked')


if __name__ == '__main__':
    main(sys.argv[1])
