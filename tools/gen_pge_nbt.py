"""Build crates/core/data/pge_nbt.json from PG&E's official Net Billing export-rate files.

Source: https://www.pge.com/eecvalues (PGE-Solar-Billing-Plan-Export-Rates.zip), one CSV per
vintage ("PG&E NBT EEC Values <YYYY> Vintage.csv"). Each row is one hour (UTC) of one component;
ValueName ("Jul Weekday HS18") gives month, day type and hour-starting in Pacific prevailing
time. RIN containing PGXX = delivery, XXPG = generation (bundled PG&E customers only).

Each cell is stored as an integer in 1/100000 $/kWh (the files have 5 decimals). Where DST
shifts make a label carry two values in a month, the most common value is kept.

Usage: python -I tools/gen_pge_nbt.py <folder with the CSVs> crates/core/data/pge_nbt.json
"""
import collections, csv, datetime, json, os, sys

MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
YEARS = range(2025, 2037)
VINTAGES = [2023, 2024, 2025, 2026]


def nth_sunday(y, m, n):
    d = datetime.datetime(y, m, 1)
    d += datetime.timedelta(days=(6 - d.weekday()) % 7)
    return d + datetime.timedelta(weeks=n - 1)


def pacific_year(u):
    y = u.year
    start = nth_sunday(y, 3, 2) + datetime.timedelta(hours=10)
    end = nth_sunday(y, 11, 1) + datetime.timedelta(hours=9)
    return (u + datetime.timedelta(hours=-7 if start <= u < end else -8)).year


def load(path):
    cells = collections.defaultdict(collections.Counter)
    with open(path, encoding='utf-8-sig', newline='') as f:
        for r in csv.DictReader(f):
            u = datetime.datetime.strptime(r['DateStart'] + ' ' + r['TimeStart'], '%m/%d/%Y %H:%M:%S')
            y = pacific_year(u)
            if y not in YEARS:
                continue
            mon, dtype, hs = r['ValueName'].split()
            comp = 'd' if 'PGXX' in r['RIN'] else 'g'
            cells[(y, comp, dtype, MON.index(mon), int(hs[2:]))][round(float(r['Value']) * 100000)] += 1
    out = {}
    for y in YEARS:
        if any(not cells[(y, c, t, m, h)] for c in 'gd' for t in ('Weekday', 'Weekend') for m in range(12) for h in range(24)):
            continue  # year not (fully) covered for this vintage
        yt = {}
        for comp in 'gd':
            for dtype, key in (('Weekday', 'wd'), ('Weekend', 'we')):
                yt[comp + '_' + key] = [[cells[(y, comp, dtype, m, h)].most_common(1)[0][0] for h in range(24)] for m in range(12)]
        out[str(y)] = yt
    return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    vint = {}
    for v in VINTAGES:
        vint[str(v)] = load(os.path.join(src, f'PG&E NBT EEC Values {v} Vintage.csv'))
        print('vintage', v, 'done', file=sys.stderr)
    # Identical vintages are stored once and referenced by year.
    data = {}
    for v, t in vint.items():
        same = next((k for k in data if isinstance(data[k], dict) and data[k] == t), None)
        data[v] = same if same else t
    doc = {
        'source': 'PG&E Solar Billing Plan (Net Billing Tariff) Energy Export Credit values, '
                  'https://www.pge.com/eecvalues (files dated 2024-12-18; CY2026 checked against the '
                  'June 2026 price sheets).',
        'units': '1/100000 USD per kWh; [month 0-11][hour-starting 0-23, Pacific prevailing time]',
        'components': 'g = generation (bundled PG&E customers only), d = delivery; total = g + d. '
                      'ACC Plus is not included.',
        'vintages': data,
    }
    with open(dst, 'w', encoding='utf-8') as f:
        json.dump(doc, f, separators=(',', ':'))
    print('wrote', dst, os.path.getsize(dst), 'bytes', {k: (v if isinstance(v, str) else 'table') for k, v in data.items()}, file=sys.stderr)


main()
