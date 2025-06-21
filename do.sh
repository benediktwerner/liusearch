#!/bin/bash

set -e

month="2022-$1"
fname="lichess_db_standard_rated_$month.pgn.zst"

echo -e "\nDownloading $month"
wget "https://database.lichess.org/standard/$fname"

echo -e "\nProcessing $month"
./extractor extract "$fname"

echo -e "\nDeleting $month"
rm "$fname"
