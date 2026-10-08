"""Daily pricing schedule, persisted by the platform; never receives credentials."""
import json
import sys

DAY = 86400
OFFSET = 8 * 3600
HOUR = 3 * 3600

def next_daily(now):
    today = ((now + OFFSET) // DAY) * DAY - OFFSET + HOUR
    return today if today > now else today + DAY

def decide(data):
    now = int(data['now'])
    state = dict(data.get('state') or {})
    result = {'enabled': True, 'run': False}
    if data['action'] == 'claim':
        state['checkedAt'] = now
        if 'nextRun' not in state:
            state.update(nextRun=next_daily(now), status='scheduled')
        if now >= state['nextRun']:
            state.update(status='running', lastAttempt=now, runId=data['runId'], nextRun=now+300)
            result.update(run=True, runId=data['runId'])
    elif data['action'] == 'finish' and data.get('runId') == state.get('runId'):
        success = data.get('success') is True
        state.update(status='succeeded' if success else 'failed', lastFinished=now,
                     nextRun=next_daily(now) if success else now+3600,
                     error=None if success else '来源读取或校验失败，现有价格保留，1 小时后重试')
        if success: state['lastSuccess'] = now
    result['state'] = state
    return result

if __name__ == '__main__':
    print(json.dumps(decide(json.load(sys.stdin))))
