#!/usr/bin/env python3
"""Leakage-conscious tabular binary classification baseline for a Kaggle-style CSV task.
Install: pip install pandas numpy scikit-learn catboost
Example:
python competition/train.py --train competition/data/train.csv --test competition/data/test.csv --sample competition/data/sample_submission.csv --target is_fraud --id-column id --metric roc_auc
"""
import argparse
import json
from pathlib import Path

import numpy as np
import pandas as pd
from catboost import CatBoostClassifier
from sklearn.metrics import average_precision_score, log_loss, roc_auc_score
from sklearn.model_selection import train_test_split


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--train", required=True)
    p.add_argument("--test", required=True)
    p.add_argument("--sample", required=True)
    p.add_argument("--target", required=True)
    p.add_argument("--id-column", default=None)
    p.add_argument("--submission-column", default=None)
    p.add_argument("--metric", choices=["roc_auc", "pr_auc", "log_loss"], required=True)
    p.add_argument("--output-dir", default="competition/output")
    p.add_argument("--seed", type=int, default=42)
    args = p.parse_args()

    train, test, sample = (pd.read_csv(x) for x in [args.train, args.test, args.sample])
    if args.target not in train:
        raise ValueError(f"Missing target column {args.target!r}")
    if train[args.target].isna().any():
        raise ValueError("Target contains missing values")
    if len(test) != len(sample):
        raise ValueError("Test and sample submission row counts differ")
    if train[args.target].nunique() != 2:
        raise ValueError("This baseline supports binary classification only")
    if args.id_column and args.id_column not in test:
        raise ValueError("ID column missing in test")

    columns = [c for c in test.columns if c in train.columns and c not in {args.target, args.id_column}]
    if not columns:
        raise ValueError("No common features found")
    if not set(columns).issubset(train.columns):
        raise ValueError("Feature mismatch")
    X, X_test = train[columns].copy(), test[columns].copy()
    y = train[args.target]
    categories = X.select_dtypes(include=["object", "category", "string"]).columns.tolist()
    for col in categories:
        X[col] = X[col].fillna("__MISSING__").astype(str)
        X_test[col] = X_test[col].fillna("__MISSING__").astype(str)
    for col in columns:
        if col not in categories and not pd.api.types.is_numeric_dtype(X[col]):
            raise ValueError(f"Unsupported feature type for {col}: {X[col].dtype}")

    # Stratified random split is appropriate only for independent, IID rows.
    # Replace with temporal/group validation when the task requires it.
    xtr, xval, ytr, yval = train_test_split(
        X, y, test_size=0.2, random_state=args.seed, stratify=y
    )
    params = dict(
        iterations=1200, depth=6, learning_rate=0.04,
        loss_function="Logloss", random_seed=args.seed,
        verbose=False, thread_count=-1
    )
    model = CatBoostClassifier(**params)
    model.fit(xtr, ytr, cat_features=categories,
              eval_set=(xval, yval), early_stopping_rounds=100)
    probabilities = model.predict_proba(xval)[:, 1]
    if args.metric == "roc_auc":
        score = roc_auc_score(yval, probabilities)
    elif args.metric == "pr_auc":
        score = average_precision_score(yval, probabilities)
    else:
        score = log_loss(yval, probabilities)

    final = CatBoostClassifier(**{**params, "iterations": max(1, model.best_iteration_ + 1)})
    final.fit(X, y, cat_features=categories)
    preds = final.predict_proba(X_test)[:, 1]
    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)

    submission_col = args.submission_column
    if submission_col is None:
        candidates = [c for c in sample.columns if c != args.id_column]
        if len(candidates) != 1:
            raise ValueError("Specify --submission-column for ambiguous sample format")
        submission_col = candidates[0]
    if submission_col not in sample.columns:
        raise ValueError("Submission column absent from sample file")
    sample[submission_col] = preds
    if args.id_column and args.id_column in sample:
        if not sample[args.id_column].reset_index(drop=True).equals(test[args.id_column].reset_index(drop=True)):
            raise ValueError("Sample IDs are not aligned with test IDs")
    sample.to_csv(out / "submission.csv", index=False)
    report = {
        "metric": args.metric, "validation_score": float(score),
        "split": "stratified_random_80_20", "seed": args.seed,
        "n_train": len(train), "n_test": len(test),
        "features": columns, "categorical_features": categories,
        "best_iteration": int(model.best_iteration_),
        "warning": "Check competition metric, validation design and submission schema before upload."
    }
    (out / "validation.json").write_text(json.dumps(report, indent=2))
    final.save_model(str(out / "model.cbm"))
    print(json.dumps(report, indent=2))
    print("Wrote", out / "submission.csv")


if __name__ == "__main__":
    main()
