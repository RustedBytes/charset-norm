from __future__ import annotations

import argparse
from glob import glob
from math import ceil
from os.path import isdir
from statistics import mean, stdev
from sys import argv
from time import perf_counter_ns

from chardet import detect as chardet_detect

from charset_normalizer import detect


def calc_percentile(data, percentile):
    n = len(data)
    p = n * percentile / 100
    sorted_data = sorted(data)

    return sorted_data[max(0, int(ceil(p)) - 1)]


def performance_compare(arguments):
    parser = argparse.ArgumentParser(
        description="Performance CI/CD check for Charset-Normalizer"
    )

    parser.add_argument(
        "-s",
        "--size-increase",
        action="store",
        default=1,
        type=int,
        dest="size_coeff",
        help="Apply artificial size increase to challenge the detection mechanism further",
    )
    parser.add_argument(
        "--max-bytes",
        type=int,
        default=None,
        help="Limit both detectors to the same input prefix",
    )

    args = parser.parse_args(arguments)
    if args.max_bytes is not None and args.max_bytes < 0:
        parser.error("--max-bytes must be greater than or equal to zero")

    if not isdir("./char-dataset"):
        print(
            "This script require https://github.com/Ousret/char-dataset to be cloned on package root directory"
        )
        exit(1)

    chardet_results = []
    charset_normalizer_results = []

    file_list = sorted(glob("./char-dataset/**/*.*"))
    total_files = len(file_list)

    # Warm deferred imports without priming either detector with corpus data.
    dummy_big5_content = b"\xa4\xa4\xa4\xe5\xb4\xfa\xb8\xd5" * 1024
    chardet_detect(dummy_big5_content)
    detect(dummy_big5_content)

    for idx, tbt_path in enumerate(file_list):
        with open(tbt_path, "rb") as fp:
            content = fp.read() * args.size_coeff
            if args.max_bytes is not None:
                content = content[: args.max_bytes]

        before = perf_counter_ns()
        chardet_detect(content)
        chardet_time = (perf_counter_ns() - before) / 1000000000
        chardet_results.append(chardet_time)

        before = perf_counter_ns()
        detect(content)
        charset_normalizer_time = (perf_counter_ns() - before) / 1000000000
        charset_normalizer_results.append(charset_normalizer_time)

        charset_normalizer_time = charset_normalizer_time or 0.000005
        cn_faster = (chardet_time / charset_normalizer_time) * 100 - 100
        print(
            f"{idx + 1:>3}/{total_files} {tbt_path:<82} C:{chardet_time:.5f}  "
            f"CN:{charset_normalizer_time:.5f}  {cn_faster:.1f} %"
        )

    # Print the top 10 rows with the slowest execution time
    print(
        f"\n{'-' * 102}\nTop 10 rows with the slowest execution time of charset_normalizer:\n"
    )
    sorted_results = sorted(
        enumerate(charset_normalizer_results), key=lambda x: x[1], reverse=True
    )
    for idx, time in sorted_results[:10]:
        tbt_path = file_list[idx]
        print(f"{idx + 1:>3}/{total_files} {tbt_path:<82}  CN:{time:.5f}")

    # Print charset normalizer statistics
    min_time = min(charset_normalizer_results)
    max_time = max(charset_normalizer_results)
    stdev_time = stdev(charset_normalizer_results)
    mean_time = mean(charset_normalizer_results)
    cv = (stdev_time / mean_time) * 100  # Coefficient of variation
    print(f"\n{'-' * 102}\nCharset Normalizer statistics:\n")
    print(f"Minimum Execution Time: {min_time:.5f} seconds")
    print(f"Maximum Execution Time: {max_time:.5f} seconds")
    print(f"Mean Execution Time: {mean_time:.5f} seconds")
    print(f"Standard Deviation: {stdev_time:.5f} seconds")
    print(f"Coefficient of Variation (CV): {cv:.1f} %")

    # Print comparison statistics for chardet and charset normalizer
    chardet_avg_delay = mean(chardet_results) * 1000
    chardet_99p = calc_percentile(chardet_results, 99) * 1000
    chardet_95p = calc_percentile(chardet_results, 95) * 1000
    chardet_50p = calc_percentile(chardet_results, 50) * 1000

    charset_normalizer_avg_delay = mean(charset_normalizer_results) * 1000
    charset_normalizer_99p = calc_percentile(charset_normalizer_results, 99) * 1000
    charset_normalizer_95p = calc_percentile(charset_normalizer_results, 95) * 1000
    charset_normalizer_50p = calc_percentile(charset_normalizer_results, 50) * 1000

    print(f"\n{'-' * 102}\nCharset Normalizer vs Chardet statistics:\n")

    print("------------------------------")
    print("--> Chardet Conclusions")
    print(f"   --> Avg: {chardet_avg_delay:.3f}ms")
    print(f"   --> 99th: {chardet_99p:.3f}ms")
    print(f"   --> 95th: {chardet_95p:.3f}ms")
    print(f"   --> 50th: {chardet_50p:.3f}ms")

    print("------------------------------")
    print("--> Charset-Normalizer Conclusions")
    print(f"   --> Avg: {charset_normalizer_avg_delay:.3f}ms")
    print(f"   --> 99th: {charset_normalizer_99p:.3f}ms")
    print(f"   --> 95th: {charset_normalizer_95p:.3f}ms")
    print(f"   --> 50th: {charset_normalizer_50p:.3f}ms")

    return 0


if __name__ == "__main__":
    exit(performance_compare(argv[1:]))
