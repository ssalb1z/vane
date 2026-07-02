#!/usr/bin/env python3
"""vane's Stage-1 demand/solar forecaster (the thin Python ML layer).

Reads a JSON request on stdin and writes a JSON forecast on stdout. Contract
(see crates/vane-forecast/src/lib.rs):

    in:  {"train": [{"demand":[24], "temp":[24], "ghi":[24]}, ...],
          "predict": {"temp":[24]}}
    out: {"demand":[24], "ghi":[24], "demand_lo":[24], "demand_hi":[24]}

The reference model is a per-hour ordinary-least-squares regression of demand on
temperature, learned across the training days, plus a per-hour mean for solar.
It is deliberately stdlib-only (no numpy/sklearn) so it runs anywhere; the JSON
boundary is unchanged if you later swap in LightGBM or a neural net.
"""

import json
import math
import sys


def ols(xs, ys):
    """Return (intercept, slope, resid_std) for y ~ x via least squares.

    Falls back to (mean, 0, std) when there are too few points or x has no
    variance — i.e. predict the mean, weather-agnostic."""
    n = len(xs)
    mean_y = sum(ys) / n
    if n < 3:
        return mean_y, 0.0, 0.0
    mean_x = sum(xs) / n
    sxx = sum((x - mean_x) ** 2 for x in xs)
    if sxx < 1e-9:
        var = sum((y - mean_y) ** 2 for y in ys) / n
        return mean_y, 0.0, math.sqrt(var)
    sxy = sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys))
    slope = sxy / sxx
    intercept = mean_y - slope * mean_x
    resid = [y - (intercept + slope * x) for x, y in zip(xs, ys)]
    var = sum(r * r for r in resid) / n
    return intercept, slope, math.sqrt(var)


def forecast(req):
    train = req["train"]
    pred_temp = req["predict"]["temp"]
    hours = len(pred_temp)

    demand, lo, hi, ghi = [], [], [], []
    for h in range(hours):
        temps = [d["temp"][h] for d in train]
        dem = [d["demand"][h] for d in train]
        intercept, slope, sd = ols(temps, dem)
        yhat = intercept + slope * pred_temp[h]
        demand.append(yhat)
        lo.append(yhat - 1.96 * sd)
        hi.append(yhat + 1.96 * sd)
        # Solar: per-hour climatological mean (irradiance ~ weather-agnostic here).
        gh = [d["ghi"][h] for d in train]
        ghi.append(sum(gh) / len(gh))

    return {"demand": demand, "ghi": ghi, "demand_lo": lo, "demand_hi": hi}


def main():
    req = json.load(sys.stdin)
    json.dump(forecast(req), sys.stdout)


if __name__ == "__main__":
    main()
