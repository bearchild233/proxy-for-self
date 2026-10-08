"""Hot-updatable backup admission and retention policy; credentials stay in the gateway."""
import json
import sys


def decide(data):
    result = {'enabled': True}
    if data['action'] == 'schedule':
        result['schedule'] = data.get('due') is True
    elif data['action'] == 'retention':
        days, count = data['retentionDays'], data['retentionCount']
        result['delete'] = [row['id'] for index, row in enumerate(data['records'])
                            if index > 0 and ((count > 0 and index >= count)
                            or (days > 0 and row['completedAt'] is not None
                                and row['completedAt'] <= data['now'] - days * 86400))][:10]
    return result


if __name__ == '__main__':
    print(json.dumps(decide(json.load(sys.stdin))))
