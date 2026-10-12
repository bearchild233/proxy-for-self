"""账号×网关策略。只处理事实，Cookie、state 和上游凭据由网关持有。"""
import math


DEFAULTS = {
    'mode': 'lazy', 'windowSeconds': 180, 'initialRestSeconds': 7200,
    'maximumRestSeconds': 86400, 'probeIntervalMs': 50, 'groupIds': [], 'gateways': [],
    'poolAccountIds': {}, 'poolUrl': 'https://pool.0102400.xyz', 'probeModel': 'gpt-6.1-sol',
}


def classify(status, state_returned, *, paired, gateway_matches, failed=False):
    if status != 200 or not paired or not gateway_matches or failed:
        return None
    return '29' if state_returned else '21'


def initial(settings, learned=None):
    return min(settings['maximumRestSeconds'], learned or settings['initialRestSeconds'])


def recover(pair, settings, waited):
    """只学习本轮最后一档；连续两轮同档成功才提高以后起点。"""
    if waited is None:
        return
    previous = pair.get('lastRecoveredRestSeconds')
    streak = pair.get('recoveryStreak', 0) + 1 if previous == waited else 1
    if previous is not None and previous != waited:
        pair['learnedRestSeconds'] = None
    pair.update(lastRecoveredRestSeconds=waited, recoveryStreak=streak)
    if streak >= 2:
        pair['learnedRestSeconds'] = waited


def retire(pair, settings, *, degraded, failed=False):
    if pair.get('state') in {'cooling', 'draining'}:
        return
    waited = pair.get('probeRestSeconds') if pair.get('state') == 'preparing' else None
    pair['restSeconds'] = (
        min(settings['maximumRestSeconds'], waited * 2)
        if (degraded or failed) and waited is not None
        else initial(settings, pair.get('learnedRestSeconds'))
    )
    pair.update(state='draining', retiredReason='29' if degraded else 'probe_failed' if failed else 'rotation')


def settle(pair):
    # 本地结束/取消之前都不算静置。访问失败也不会凭空获得静置时间。
    if pair.get('state') == 'draining' and not pair.get('inFlight'):
        if pair.get('lastContactAt') is None:
            pair.update(state='unknown', restUntil=None)
        else:
            pair.update(state='cooling', restUntil=pair['lastContactAt'] + pair['restSeconds'])


def rotation_due(pair, settings, now, *, demand):
    return (
        pair.get('state') == 'ready'
        and now >= pair['firstContactAt'] + settings['windowSeconds']
        and (demand or settings['mode'] == 'continuous')
    )


def finite_seconds(value, name, maximum):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 1 <= value <= maximum:
        raise ValueError(name + '无效')
    return float(value)
